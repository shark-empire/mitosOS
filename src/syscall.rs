//! System Call Dispatcher and Hardware Trampoline layer for mitosOS.
//!
//! Provides ring 0/ring 3 memory boundary enforcement, POSIX-compatible 
//! syscall dispatching, and hardware MSR/exception vector entry glue 
//! for x86_64 and AArch64 targets.

use core::fmt::Write;
use crate::version::UtsName;

// =========================================================================
// System Call Numbers
// =========================================================================
pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_MUNMAP: usize = 11;
pub const SYS_EXIT: usize = 60;
pub const SYS_UNAME: usize = 63;

// =========================================================================
// Central Dispatcher
// =========================================================================

/// Central kernel entry point for system calls dispatched from low-level assembly glue.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_handler(
    sys_num: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
) -> usize {
    match sys_num {
        SYS_WRITE => sys_write(arg1, arg2 as *const u8, arg3),
        SYS_READ => sys_read(arg1, arg2 as *mut u8, arg3),
        SYS_MUNMAP => sys_munmap(arg1, arg2),
        SYS_EXIT => sys_exit(arg1),
        SYS_UNAME => sys_uname(arg1 as *mut UtsName),
        _ => sys_unknown(sys_num),
    }
}

// =========================================================================
// System Call Handlers & Validation
// =========================================================================

/// Returns true if it's safe for the kernel to read (`need_write = false`) 
/// or write into (`need_write = true`) `len` bytes starting at `ptr`, on behalf 
/// of whichever task is currently making this syscall.
///
/// A SharedThread (kernel-mode) caller passes its own plain kernel-address 
/// stack locals and is trusted unconditionally. Only a genuine ring-3 
/// `IsolatedProcess` caller gets its pointer walked against its own page table.
fn validate_user_ptr(ptr: usize, len: usize, need_write: bool) -> bool {
    let (root, is_ring3) = crate::task::current_task_access_info();
    if !is_ring3 {
        return true;
    }
    crate::vmm::validate_user_range(root, ptr, len, need_write)
}

/// Writes raw byte buffers to standard output (1) or standard error (2).
fn sys_write(fd: usize, ptr: *const u8, len: usize) -> usize {
    if (fd != 1 && fd != 2) || ptr.is_null() || len == 0 {
        return usize::MAX;
    }
    if !validate_user_ptr(ptr as usize, len, false) {
        return usize::MAX;
    }

    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    let mut uart = crate::uart::Uart::shared();

    // Stream raw bytes directly to UART to support both UTF-8 strings and binary output
    for &byte in slice {
        let _ = uart.write_char(byte as char);
    }

    len
}

/// Reads input from standard input (0) into a target buffer.
fn sys_read(fd: usize, ptr: *mut u8, len: usize) -> usize {
    if fd != 0 || ptr.is_null() || len == 0 {
        return usize::MAX;
    }
    if !validate_user_ptr(ptr as usize, len, true) {
        return usize::MAX;
    }

    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
    let mut bytes_read = 0;

    while bytes_read < len {
        if let Some(byte) = crate::interrupts::dequeue_byte() {
            slice[bytes_read] = byte;
            bytes_read += 1;
        } else {
            break;
        }
    }

    bytes_read
}

/// Populates system information metadata into the provided `UtsName` buffer pointer.
fn sys_uname(ptr: *mut UtsName) -> usize {
    if ptr.is_null() {
        return usize::MAX;
    }
    if !validate_user_ptr(ptr as usize, core::mem::size_of::<UtsName>(), true) {
        return usize::MAX;
    }

    // Safety: Pointer validity and writable bounds verified above
    let uts = unsafe { &mut *ptr };
    uts.populate();

    0 // Success
}

/// Releases `len` bytes of the calling process's own memory starting at `ptr`, 
/// returning each page's physical frame to the allocator. Both `ptr` and `len` 
/// must be page-aligned.
fn sys_munmap(ptr: usize, len: usize) -> usize {
    // Check page alignment and underflow/overflow bounds
    if ptr & 0xFFF != 0 || len == 0 || len & 0xFFF != 0 {
        return usize::MAX;
    }

    let end_addr = match ptr.checked_add(len) {
        Some(end) => end,
        None => return usize::MAX,
    };

    let (root, is_ring3) = crate::task::current_task_access_info();
    if !is_ring3 || !crate::vmm::validate_user_range(root, ptr, len, false) {
        return usize::MAX;
    }

    let root_ptr = root as *mut crate::vmm::arch::PageTable;
    let mut addr = ptr;

    while addr < end_addr {
        unsafe {
            if let Some(phys) = crate::vmm::arch::translate_addr(root_ptr as *const _, addr) {
                if crate::vmm::arch::unmap_page(root_ptr, addr).is_ok() {
                    crate::memory::vmm_free_frame(phys);
                }
            }
        }
        addr += 0x1000;
    }
    0
}

/// Terminates the current execution task.
fn sys_exit(_exit_code: usize) -> ! {
    crate::task::exit();
}

/// Fallback for unregistered or unhandled system call numbers.
fn sys_unknown(sys_num: usize) -> usize {
    let mut uart = crate::uart::Uart::shared();
    let _ = writeln!(uart, "mitosOS: Unknown syscall number: {sys_num}");
    usize::MAX
}

// =========================================================================
// HARDWARE INITIALIZATION & ASSEMBLY ENTRY STUBS
// =========================================================================

/// Configures CPU hardware registers and model-specific registers (MSRs) 
/// to route ring 3 syscall interrupts directly into `syscall_handler`.
pub fn init_syscall_hardware() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        const MSR_EFER: u32 = 0xC000_0080;
        const MSR_STAR: u32 = 0xC000_0081;
        const MSR_LSTAR: u32 = 0xC000_0082;
        const MSR_FMASK: u32 = 0xC000_0084;

        // 1. Enable System Call Extensions (SCE) in EFER
        let efer = read_msr(MSR_EFER);
        write_msr(MSR_EFER, efer | 1);

        // 2. Setup STAR register with GDT Selectors (CS=0x08, SS=0x10, User CS=0x1B, User SS=0x23)
        let star = ((0x0008u64) << 32) | ((0x0010u64) << 48);
        write_msr(MSR_STAR, star);

        // 3. Point LSTAR to low-level assembly trampoline
        write_msr(MSR_LSTAR, x86_64_syscall_entry as usize as u64);

        // 4. Clear Interrupt Flag (RFLAGS bit 9) upon syscall entry
        write_msr(MSR_FMASK, 0x0200);
    }

    #[cfg(target_arch = "aarch64")]
    {
        // On AArch64, 'svc #0' is automatically trapped via vector_table EL0 synchronous exceptions
    }
}

// Low-level MSR helper utilities for x86_64
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") low,
        out("edx") high,
        options(nomem, nostack)
    );
    ((high as u64) << 32) | (low as u64)
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn write_msr(msr: u32, val: u64) {
    let low = val as u32;
    let high = (val >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") low,
        in("edx") high,
        options(nomem, nostack)
    );
}

// Low-level assembly trampoline for x86_64 fast syscall execution
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".global x86_64_syscall_entry",
    "x86_64_syscall_entry:",
    "    // Preserve caller registers on kernel stack",
    "    push r11",
    "    push rcx",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "",
    "    // Map ABI registers: RAX -> RDI (sys_num), RDI -> RSI (arg1), RSI -> RDX (arg2), RDX -> RCX (arg3)",
    "    mov rcx, rdx",
    "    mov rdx, rsi",
    "    mov rsi, rdi",
    "    mov rdi, rax",
    "",
    "    call syscall_handler",
    "",
    "    // Restore caller state and return to user mode",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    pop rcx",
    "    pop r11",
    "    sysretq"
);
