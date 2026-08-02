//! System Call Dispatcher layer for mitosOS.

use core::fmt::Write;
use crate::version::UtsName;

// =========================================================================
// System Call Numbers
// =========================================================================
pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_EXIT: usize = 60;
pub const SYS_UNAME: usize = 63;

// =========================================================================
// Central Dispatcher
// =========================================================================

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
        SYS_EXIT => sys_exit(arg1),
        SYS_UNAME => sys_uname(arg1 as *mut UtsName),
        _ => sys_unknown(sys_num),
    }
}

// =========================================================================
// System Call Handlers
// =========================================================================

/// Returns true if it's safe for the kernel to read (`need_write =
/// false`) or write into (`need_write = true`) `len` bytes starting at
/// `ptr`, on behalf of whichever task is currently making this
/// syscall.
///
/// A SharedThread (kernel-mode) caller -- e.g. shell.rs's `cmd_uname`,
/// which invokes this same `int 0x80`/`svc #0` path directly from
/// ring 0 -- passes its own plain kernel-address stack locals; those
/// were never "user" pages to begin with, so it's trusted
/// unconditionally, same as before. Only a genuine ring-3
/// `IsolatedProcess` caller gets its pointer walked against its own
/// page table: without this, any userspace program could pass a
/// kernel address to write()/read() and use the kernel's own,
/// previously-unchecked `core::slice::from_raw_parts` as an arbitrary
/// kernel-memory read/write primitive.
fn validate_user_ptr(ptr: usize, len: usize, need_write: bool) -> bool {
    let (root, is_ring3) = crate::task::current_task_access_info();
    if !is_ring3 {
        return true;
    }
    crate::vmm::validate_user_range(root, ptr, len, need_write)
}

/// Writes data from a buffer to standard output (1) or standard error (2).
fn sys_write(fd: usize, ptr: *const u8, len: usize) -> usize {
    if (fd != 1 && fd != 2) || ptr.is_null() || len == 0 {
        return usize::MAX;
    }
    if !validate_user_ptr(ptr as usize, len, false) {
        return usize::MAX;
    }

    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    let mut uart = crate::uart::Uart::shared();

    if let Ok(text) = core::str::from_utf8(slice) {
        let _ = uart.write_str(text);
        len
    } else {
        usize::MAX
    }
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

    // Safety: Verify pointer is non-null before writing
    let uts = unsafe { &mut *ptr };
    uts.populate();

    0 // Success
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
