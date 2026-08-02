//! Process Management and Privilege Level Transition Engine for mitosOS.
//!
//! Handles Process Control Block (PCB) allocations, isolated user stack setup,
//! and the architecture-specific privilege drop to Ring 3 (x86_64) or EL0 (AArch64).

use core::sync::atomic::{AtomicU64, Ordering};

/// Unique identifier for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub u64);

impl ProcessId {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        ProcessId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Execution state of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Exited(i32),
}

/// Process Control Block (PCB) representing an isolated user-space application.
pub struct Process {
    pub id: ProcessId,
    pub state: ProcessState,
    pub page_table_root: usize,
    pub user_stack_top: usize,
    pub entry_point: usize,
}

impl Process {
    /// Creates a new process structure without launching it.
    pub fn new(page_table_root: usize, entry_point: usize, user_stack_top: usize) -> Self {
        Self {
            id: ProcessId::new(),
            state: ProcessState::Ready,
            page_table_root,
            user_stack_top,
            entry_point,
        }
    }
}

/// Default virtual address top for user stacks (0x0000_7FFF_FFFF_0000).
pub const USER_STACK_TOP_DEFAULT: usize = 0x0000_7FFF_FFFF_0000;

/// Allocates and maps a non-executable (NX) user-mode stack growing downwards.
pub fn allocate_user_stack(
    page_table_root: usize,
    stack_top_vaddr: usize,
    num_pages: usize,
) -> Result<usize, &'static str> {
    const PAGE_SIZE: usize = 4096;

    let flags = crate::memory::MapFlags {
        writable: true,
        user_accessible: true,
        execute_disable: true, // Security: Non-executable stack (NX/XN)
        device: false,
    };

    let root = page_table_root as *mut crate::vmm::arch::PageTable;

    // Allocate stack pages downwards from stack_top_vaddr
    for i in (0..num_pages).rev() {
        let page_vaddr = stack_top_vaddr
            .checked_sub((i + 1) * PAGE_SIZE)
            .ok_or("User stack virtual address underflow")?;

        if page_vaddr < crate::memory::USER_SPACE_BASE {
            return Err("User stack location bleeds into kernel address space");
        }

        let frame = crate::memory::vmm_alloc_frame()
            .ok_or("Out of physical memory allocating user stack frame")?;

        unsafe {
            // Zero stack frame to prevent kernel data leakage to user mode
            core::ptr::write_bytes(frame as *mut u8, 0, PAGE_SIZE);

            crate::vmm::arch::map_page(root, page_vaddr, frame, flags)
                .map_err(|_| "Failed to map user stack page into page table")?;
        }
    }

    Ok(stack_top_vaddr)
}

/// Spawns an ELF binary into a new isolated process space and executes it in User Mode.
///
/// THIS FUNCTION NEVER RETURNS IF SUCCESSFUL.
pub fn spawn_and_run_elf(elf_binary: &[u8]) -> Result<!, &'static str> {
    // 1. Create isolated page table hierarchy
    let page_table_root = crate::vmm::create_process_page_table()?;

    // 2. Load executable segments into user space
    let entry_point = crate::elf::load_elf_to_process(elf_binary, page_table_root)?;

    // 3. Allocate 16 KB User Stack (4 pages)
    let stack_top = allocate_user_stack(page_table_root, USER_STACK_TOP_DEFAULT, 4)?;

    // 4. Perform privilege drop and enter user mode
    unsafe {
        enter_user_mode(entry_point, stack_top, page_table_root);
    }
}

/// Switches address space and drops CPU execution privilege to Ring 3 (x86_64) or EL0 (AArch64).
///
/// # Safety
/// Caller must ensure `entry_point`, `user_stack_top`, and `page_table_root` are valid user-space mappings.
pub unsafe fn enter_user_mode(
    entry_point: usize,
    user_stack_top: usize,
    page_table_root: usize,
) -> ! {
    #[cfg(target_arch = "x86_64")]
    {
        // Standard GDT Selectors with RPL = 3 (Ring 3 User Mode)
        const USER_CS: u64 = 0x1B; // 0x18 | 3
        const USER_DS: u64 = 0x23; // 0x20 | 3
        const RFLAGS_IF: u64 = 0x202; // Enable interrupts in user mode

        // 1. Activate process page table (CR3)
        core::arch::asm!(
            "mov cr3, {}",
            in(reg) page_table_root,
            options(nostack)
        );

        // 2. Build iretq stack frame: [SS, RSP, RFLAGS, CS, RIP] and execute iretq
        core::arch::asm!(
            "mov ds, {0:r}",
            "mov es, {0:r}",
            "mov fs, {0:r}",
            "mov gs, {0:r}",
            "push {0}",          // SS
            "push {1}",          // RSP
            "push {2}",          // RFLAGS
            "push {3}",          // CS
            "push {4}",          // RIP
            "iretq",
            in(reg) USER_DS,
            in(reg) user_stack_top,
            in(reg) RFLAGS_IF,
            in(reg) USER_CS,
            in(reg) entry_point,
            options(noreturn)
        );
    }

    #[cfg(target_arch = "aarch64")]
    {
        // 1. Activate process page table (TTBR0_EL1) & flush local TLB
        core::arch::asm!(
            "msr ttbr0_el1, {}",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            in(reg) page_table_root,
            options(nostack)
        );

        // 2. Set ELR_EL1 (target PC), SP_EL0 (user stack), SPSR_EL1 (0x0 = EL0t + unmask DAIF)
        core::arch::asm!(
            "msr elr_el1, {}",
            "msr sp_el0, {}",
            "msr spsr_el1, xzr",
            "eret",
            in(reg) entry_point,
            in(reg) user_stack_top,
            options(noreturn)
        );
    }
}
