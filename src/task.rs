//! Task management and scheduler for mitosOS.
//!
//! Features an O(1) cache-aligned Execution Engine supporting both
//! isolated processes and shared-memory threads natively.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use alloc::vec::Vec;

const STACK_SIZE: usize = 8192; // 8KB stack per task
const MAX_TASKS: usize = 4;

// ==========================================
// Execution Modes & Task State
// ==========================================

/// Defines how a new task interacts with system memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Process: Allocates a completely new, isolated hardware page table.
    IsolatedProcess,
    /// Thread: Shares the exact hardware page table (virtual memory) of the parent.
    SharedThread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// A standard 32-byte message passed between isolated processes.
#[derive(Debug, Clone, Copy)]
pub struct Message {
    pub sender_id: usize,
    pub data: [u8; 32],
}

// ==========================================
// Hardware Context Definitions
// ==========================================

/// Architecture-specific hardware context pushed by exceptions/interrupts.
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TaskContext {
    pub r15: usize, pub r14: usize, pub r13: usize, pub r12: usize,
    pub r11: usize, pub r10: usize, pub r9: usize,  pub r8: usize,
    pub rbp: usize, pub rdi: usize, pub rsi: usize, pub rdx: usize,
    pub rcx: usize, pub rbx: usize, pub rax: usize,
    pub rip: usize, pub cs: usize, pub rflags: usize, pub rsp: usize, pub ss: usize,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TaskContext {
    pub regs: [usize; 31], // x0 through x30
    pub spsr: usize,       
    pub elr: usize,        
    pub _pad: usize,       
}

/// A 16-byte aligned stack wrapper.
#[repr(C, align(16))]
struct TaskStack([u8; STACK_SIZE]);

// ==========================================
// Cache-Aligned Task Control Block
// ==========================================

/// Represents a single CPU execution context.
/// 
/// `align(64)` forces the struct to perfectly fit inside a standard CPU cache line.
/// This prevents "false sharing" across CPU cores, maximizing scheduler speed.
#[repr(C, align(64))]
pub struct Task {
    pub id: usize,
    pub fd_table: Option<crate::fd::FileDescriptorTable>,
    pub parent_id: usize,
    pub sp: usize,
    /// Hardware Page Table Root (CR3 on x86_64, TTBR0_EL1 on AArch64).
    pub memory_root: usize, 
    pub state: TaskState,
    pub mailbox: Option<Message>, 
    stack: TaskStack,
    /// This task's own kernel-mode stack. A `SharedThread` never uses
    /// anything else -- unchanged from before this field existed. An
    /// `IsolatedProcess` running in ring 3 uses `stack` as its *user*
    /// context's launch pad (see Task::init) and this as the stack the
    /// CPU switches to (via TSS.RSP0) on any trap taken while it's in
    /// ring 3. Needs to be per-task: two ring-3 tasks sharing one kernel
    /// stack could stomp on each other the moment one of them yields or
    /// blocks mid-trap (e.g. inside a blocking syscall) and the
    /// scheduler picks the other.
    #[cfg(target_arch = "x86_64")]
    kernel_stack: TaskStack,
}

impl Task {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            parent_id: 0,
            sp: 0,
            memory_root: 0,
            state: TaskState::Terminated,
            mailbox: None,
            stack: TaskStack([0; STACK_SIZE]),
            fd_table: None,
            #[cfg(target_arch = "x86_64")]
            kernel_stack: TaskStack([0; STACK_SIZE]),
        }
    }



    /// Initializes the stack frame, registers, and memory boundaries for a new task.
    ///
    /// `user_stack_top` is only meaningful for `ExecutionMode::IsolatedProcess`
    /// -- pass `0` for `SharedThread` (kernel-mode tasks don't have one).
    pub fn init(
        &mut self, 
        id: usize, 
        entry: extern "C" fn() -> !, 
        mode: ExecutionMode, 
        parent_memory_root: usize,
        user_stack_top: usize,
    ) {
        self.id = id;
        self.parent_id = if mode == ExecutionMode::SharedThread { id } else { id };
        self.state = TaskState::Ready;
        self.fd_table = Some(crate::fd::FileDescriptorTable::new()); 

        self.memory_root = match mode {
            ExecutionMode::SharedThread => parent_memory_root,
            ExecutionMode::IsolatedProcess => {
                allocate_isolated_page_table(parent_memory_root)
            }
        };

        // `stack` is where the very first resume-frame lives either way
        // -- that's what makes the *first* switch into any task, ring-3
        // or not, go through the exact same generic scheduler path as
        // any other switch (see run_schedule). For a ring-3 task, `stack`
        // becomes its kernel_stack in every sense after this: it's what
        // TSS.RSP0 will point at (run_schedule sets that on every
        // switch), and the CPU only ever touches it again on a trap back
        // into the kernel -- actual execution happens on user_stack_top.
        let stack_top = self.stack.0.as_ptr() as usize + STACK_SIZE;
        let aligned_top = stack_top & !0xF;
        let frame_ptr = (aligned_top - core::mem::size_of::<TaskContext>()) as *mut TaskContext;

        #[cfg(target_arch = "x86_64")]
        unsafe {
            let is_user = mode == ExecutionMode::IsolatedProcess && user_stack_top != 0;
            frame_ptr.write(TaskContext {
                r15: 0, r14: 0, r13: 0, r12: 0,
                r11: 0, r10: 0, r9: 0,  r8: 0,
                rbp: 0, rdi: 0, rsi: 0, rdx: 0,
                rcx: 0, rbx: 0, rax: 0,
                rip: entry as usize,
                cs: if is_user { crate::gdt::USER_CODE_SELECTOR as usize } else { 0x08 },
                rflags: 0x202,
                rsp: if is_user { user_stack_top } else { stack_top },
                ss: if is_user { crate::gdt::USER_DATA_SELECTOR as usize } else { 0x10 },
            });
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            // AArch64 ring-3 (EL0) execution isn't wired up yet -- every
            // task still runs at EL1h regardless of `mode`. The
            // synchronous-exception vector for traps from EL0 already
            // exists (interrupts.rs), so this is mainly SPSR/TTBR0 work
            // when it's time to do this architecture too.
            let _ = user_stack_top;
            frame_ptr.write(TaskContext {
                regs: [0; 31],
                spsr: 0x05,            
                elr: entry as usize,   
                _pad: 0,
            });
        }

        self.sp = frame_ptr as usize;
    }

    /// Top of this task's kernel stack -- the value TSS.RSP0 should hold
    /// while this task is the one running (see run_schedule).
    #[cfg(target_arch = "x86_64")]
    fn kernel_stack_top(&self) -> usize {
        (self.kernel_stack.0.as_ptr() as usize + STACK_SIZE) & !0xF
    }
}

/// Gets the ID of the currently executing task.
pub fn current_task_id() -> usize {
    CURRENT_TASK.load(Ordering::Relaxed)
}

/// Sends a message to a destination task and wakes it up if it was asleep.
pub fn send_message(dest_id: usize, message_data: [u8; 32]) -> Result<(), &'static str> {
    unsafe {
        let sender_id = current_task_id();
        let tasks_ptr = core::ptr::addr_of_mut!(TASKS);
        
        for task in (*tasks_ptr).iter_mut() {
            if task.id == dest_id && task.state != TaskState::Terminated {
                if task.mailbox.is_some() {
                    return Err("Destination mailbox is full");
                }
                
                task.mailbox = Some(Message { sender_id, data: message_data });
                
                // Wake up the task if it was waiting for a message
                if task.state == TaskState::Blocked {
                    task.state = TaskState::Ready;
                }
                return Ok(());
            }
        }
    }
    Err("Destination task not found")
}

/// Reads a message. If the mailbox is empty, blocks the task until one arrives.
pub fn receive_message() -> Option<Message> {
    unsafe {
        let current_id = current_task_id();
        let tasks_ptr = core::ptr::addr_of_mut!(TASKS);
        
        for task in (*tasks_ptr).iter_mut() {
            if task.id == current_id {
                if let Some(msg) = task.mailbox.take() {
                    return Some(msg);
                } else {
                    // Put the task to sleep so the scheduler skips it
                    task.state = TaskState::Blocked;
                    crate::task::yield_now(); // Force an immediate context switch
                    return None;
                }
            }
        }
    }
    None
}

// ==========================================
// Kernel Scheduler State
// ==========================================

static mut TASKS: [Task; MAX_TASKS] = [
    Task::empty(), Task::empty(), Task::empty(), Task::empty(),
];

static CURRENT_TASK: AtomicUsize = AtomicUsize::new(0);
static TASK_INITIALIZED: AtomicBool = AtomicBool::new(false);

// ==========================================
// Public Scheduler API
// ==========================================

/// Voluntarily yield the remaining CPU timeslice to the next ready task.
pub fn yield_now() {
    #[cfg(target_arch = "x86_64")]
    unsafe { core::arch::asm!("int 0x20", options(nomem, nostack)); }

    #[cfg(target_arch = "aarch64")]
    unsafe { core::arch::asm!("svc #0", options(nomem, nostack)); }
}

/// Helper to read the CPU's current memory root.
fn current_memory_root() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        let cr3: usize;
        unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)) };
        cr3
    }
    #[cfg(target_arch = "aarch64")]
    {
        let ttbr0: usize;
        unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack)) };
        ttbr0
    }
}

/// Prepares a new task with its own stack and initial entry point function.
/// `user_stack_top` is only meaningful for `ExecutionMode::IsolatedProcess`
/// -- pass `0` for ordinary kernel-mode (`SharedThread`) tasks.
pub fn spawn(entry_point: extern "C" fn() -> !, mode: ExecutionMode, user_stack_top: usize) -> bool {
    unsafe {
        let parent_root = current_memory_root();

        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                TASKS[i].init(i, entry_point, mode, parent_root, user_stack_top);

                if !TASK_INITIALIZED.load(Ordering::Acquire) {
                    TASKS[0].state = TaskState::Running;
                    TASK_INITIALIZED.store(true, Ordering::Release);
                }

                return true;
            }
        }
    }
    false
}



/// Spawns a task whose entry point is only known at runtime -- e.g. loaded
/// from an ELF image -- rather than a named Rust function. The transmute is
/// valid because callers (currently only the ELF loader) have already
/// verified this is x86_64/aarch64 machine code that never returns.
pub fn spawn_at(entry_addr: usize, mode: ExecutionMode, user_stack_top: usize) -> bool {
    let entry_point: extern "C" fn() -> ! = unsafe { core::mem::transmute(entry_addr) };
    spawn(entry_point, mode, user_stack_top)
}


/// Allocates or clones a new page table root structure for isolated processes.
fn allocate_isolated_page_table(parent_root: usize) -> usize {
    unsafe {
        crate::memory::create_process_page_table().unwrap_or(parent_root)
    }
    
}

/// Maps a small stack for an isolated process's ring-3 code, in that
/// process's *own* page table -- user-accessible, writable, non-executable.
/// There wasn't one at all before this; every task ran on `stack` (plain
/// kernel memory, never user-accessible) regardless of mode. Returns the
/// *top* of the stack (stacks grow down towards lower addresses).
///
/// The address is fixed and arbitrary for now -- chosen well clear of
/// where an ELF is conventionally loaded (0x400000+) so the two don't
/// collide for a single process. It does *not* yet account for two
/// processes both wanting this same address (see the aliasing note on
/// create_process_page_table in memory.rs) -- fine for one process at a
/// time, worth revisiting alongside that.
#[cfg(target_arch = "x86_64")]
fn allocate_user_stack(page_table_root: usize) -> Option<usize> {
    const USER_STACK_TOP: usize = 0x1000_0000; // 256MB
    const USER_STACK_PAGES: usize = 4; // 16KB
    const PAGE_SIZE: usize = 4096;

    let root = page_table_root as *mut crate::vmm::arch::PageTable;
    let flags = crate::memory::MapFlags {
        writable: true,
        user_accessible: true,
        execute_disable: true,
    };

    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
    for i in 0..USER_STACK_PAGES {
        let vaddr = stack_bottom + i * PAGE_SIZE;
        let phys = crate::memory::vmm_alloc_frame()?;
        unsafe {
            // Zeroed for the same reason elf.rs zeroes segment frames --
            // otherwise a fresh stack starts out full of whatever
            // garbage was already in that physical frame.
            core::ptr::write_bytes(phys as *mut u8, 0, PAGE_SIZE);
            crate::vmm::arch::map_page(root, vaddr, phys, flags).ok()?;
        }
    }

    Some(USER_STACK_TOP)
}

/// AArch64 ring-3 (EL0) execution isn't wired up yet -- see the note in
/// Task::init. Nothing calls spawn_from_elf on this architecture yet, so
/// this just makes it fail cleanly (`None`) instead of silently mapping a
/// stack that would never actually get used from EL0.
#[cfg(target_arch = "aarch64")]
fn allocate_user_stack(_page_table_root: usize) -> Option<usize> {
    None
}

/// Spawns a new isolated process from an ELF binary in memory.
pub fn spawn_from_elf(elf_bytes: &[u8]) -> bool {
    let parent_root = current_memory_root();
    
    // 1. Create a new memory space for the process
    let page_table_root = allocate_isolated_page_table(parent_root);
    
    // 2. Load the ELF into that new memory space
    let entry_point = match crate::elf::load_elf_to_process(elf_bytes, page_table_root) {
        Ok(ep) => ep,
        Err(_e) => return false,
    };

    // 3. Give it a real ring-3 stack, mapped into its own page table --
    //    see allocate_user_stack for why this didn't exist before.
    let user_stack_top = match allocate_user_stack(page_table_root) {
        Some(top) => top,
        None => return false,
    };

    // 4. Spawn the task using the entry address returned by the ELF loader
    //    and the stack just mapped for it.
    spawn_at(entry_point, ExecutionMode::IsolatedProcess, user_stack_top)
}



/// Public task metadata structure for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct TaskInfo {
    pub id: usize,
    pub parent_id: usize,
    pub state: TaskState,
    pub memory_root: usize,
}

/// Safely queries active tasks for diagnostic tools like `ps`.
pub fn get_task_list() -> Vec<TaskInfo> {
    let mut list = Vec::new();
    unsafe {
        for task in (*core::ptr::addr_of!(TASKS)).iter() {
            if task.state != TaskState::Terminated {
                list.push(TaskInfo {
                    id: task.id,
                    parent_id: task.parent_id,
                    state: task.state,
                    memory_root: task.memory_root,
                });
            }
        }
    }
    list
}

/// Terminate the currently running task and yield control back to the scheduler.
pub fn exit() -> ! {
    unsafe {
        let current_idx = CURRENT_TASK.load(Ordering::Relaxed);
        TASKS[current_idx].state = TaskState::Terminated;
    }

    yield_now();

    loop { core::hint::spin_loop(); }
}

/// The core scheduling logic called by interrupts.rs on every timer tick.
#[unsafe(no_mangle)]
pub extern "C" fn run_schedule(current_sp: usize) -> usize {
    if !TASK_INITIALIZED.load(Ordering::Relaxed) {
        return current_sp;
    }

    unsafe {
        let current_idx = CURRENT_TASK.load(Ordering::Relaxed);

        if current_sp != 0 && TASKS[current_idx].state == TaskState::Running {
            TASKS[current_idx].sp = current_sp;
            TASKS[current_idx].state = TaskState::Ready;
        }

        let mut next_idx = current_idx;
        for _ in 0..MAX_TASKS {
            next_idx = (next_idx + 1) % MAX_TASKS;
            if TASKS[next_idx].state == TaskState::Ready {
                TASKS[next_idx].state = TaskState::Running;
                CURRENT_TASK.store(next_idx, Ordering::Relaxed);

                // --- Hardware Address Space Switch ---
                let next_root = TASKS[next_idx].memory_root;
                if next_root != TASKS[current_idx].memory_root && next_root != 0 {
                    #[cfg(target_arch = "x86_64")]
                    core::arch::asm!("mov cr3, {}", in(reg) next_root, options(nostack, preserves_flags));
                    
                    #[cfg(target_arch = "aarch64")]
                    core::arch::asm!("msr ttbr0_el1, {}; isb", in(reg) next_root, options(nostack, preserves_flags));
                }

                // RSP0 has to reflect whichever task is about to run,
                // every switch -- not just when the address space
                // changes. A kernel-mode (SharedThread) task never
                // actually consults it (same-privilege traps don't
                // switch stacks), but a ring-3 task's very first trap
                // back into the kernel needs this already pointing at
                // *its* kernel_stack, not whatever was there from the
                // previously running task.
                #[cfg(target_arch = "x86_64")]
                crate::gdt::set_kernel_stack(TASKS[next_idx].kernel_stack_top() as u64);

                return TASKS[next_idx].sp;
            }
        }

        if TASKS[current_idx].state == TaskState::Ready {
            TASKS[current_idx].state = TaskState::Running;
        }

        current_sp
    }
}
