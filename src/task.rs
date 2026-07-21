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
struct TaskContext {
    r15: usize, r14: usize, r13: usize, r12: usize,
    r11: usize, r10: usize, r9: usize,  r8: usize,
    rbp: usize, rdi: usize, rsi: usize, rdx: usize,
    rcx: usize, rbx: usize, rax: usize,
    rip: usize, cs: usize, rflags: usize, rsp: usize, ss: usize,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct TaskContext {
    regs: [usize; 31], // x0 through x30
    spsr: usize,       
    elr: usize,        
    _pad: usize,       
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
    pub parent_id: usize,
    pub sp: usize,
    /// Hardware Page Table Root (CR3 on x86_64, TTBR0_EL1 on AArch64).
    pub memory_root: usize, 
    pub state: TaskState,
    pub mailbox: Option<Message>, 
    stack: TaskStack,
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
        }
    }

    /// Initializes the stack frame, registers, and memory boundaries for a new task.
    pub fn init(
        &mut self, 
        id: usize, 
        entry: extern "C" fn() -> !, 
        mode: ExecutionMode, 
        parent_memory_root: usize
    ) {
        self.id = id;
        self.parent_id = if mode == ExecutionMode::SharedThread { id } else { id };
        self.state = TaskState::Ready;

                self.memory_root = match mode {
            ExecutionMode::SharedThread => parent_memory_root,
            ExecutionMode::IsolatedProcess => {
                allocate_isolated_page_table(parent_memory_root)
            }
        };



        let stack_top = self.stack.0.as_ptr() as usize + STACK_SIZE;
        let aligned_top = stack_top & !0xF;
        let frame_ptr = (aligned_top - core::mem::size_of::<TaskContext>()) as *mut TaskContext;

        #[cfg(target_arch = "x86_64")]
        unsafe {
            frame_ptr.write(TaskContext {
                r15: 0, r14: 0, r13: 0, r12: 0,
                r11: 0, r10: 0, r9: 0,  r8: 0,
                rbp: 0, rdi: 0, rsi: 0, rdx: 0,
                rcx: 0, rbx: 0, rax: 0,
                rip: entry as usize,
                cs: 0x08,              
                rflags: 0x202,         
                rsp: stack_top,
                ss: 0x10,              
            });
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            frame_ptr.write(TaskContext {
                regs: [0; 31],
                spsr: 0x05,            
                elr: entry as usize,   
                _pad: 0,
            });
        }

        self.sp = frame_ptr as usize;
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
pub fn spawn(entry_point: extern "C" fn() -> !, mode: ExecutionMode) -> bool {
    unsafe {
        let parent_root = current_memory_root();

        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                TASKS[i].init(i, entry_point, mode, parent_root);

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

/// Allocates or clones a new page table root structure for isolated processes.
/// Allocates or clones a new page table root structure for isolated processes.
fn allocate_isolated_page_table(parent_root: usize) -> usize {
    unsafe {
        crate::memory::create_process_page_table().unwrap_or(parent_root)
    }
}




// Add this public structure to src/task.rs
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
                // If the next task operates in a different memory space, swap page tables natively.
                let next_root = TASKS[next_idx].memory_root;
                if next_root != TASKS[current_idx].memory_root && next_root != 0 {
                    #[cfg(target_arch = "x86_64")]
                    core::arch::asm!("mov cr3, {}", in(reg) next_root, options(nostack, preserves_flags));
                    
                    #[cfg(target_arch = "aarch64")]
                    core::arch::asm!("msr ttbr0_el1, {}; isb", in(reg) next_root, options(nostack, preserves_flags));
                }

                return TASKS[next_idx].sp;
            }
        }

        if TASKS[current_idx].state == TaskState::Ready {
            TASKS[current_idx].state = TaskState::Running;
        }

        current_sp
    }
}
