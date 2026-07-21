//! Task management and scheduler for mitosOS.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const STACK_SIZE: usize = 8192; // Upgraded to 8KB for safety
const MAX_TASKS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Terminated,
}

/// Architecture-specific hardware context pushed by exceptions/interrupts.
/// 
/// We map the stack frame exactly as the assembly handlers push it.
/// Stacks grow down, so the first struct field is the lowest memory address.
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct TaskContext {
    // General Purpose Registers (Matches timer_handler_stub push order)
    r15: usize, r14: usize, r13: usize, r12: usize,
    r11: usize, r10: usize, r9: usize,  r8: usize,
    rbp: usize, rdi: usize, rsi: usize, rdx: usize,
    rcx: usize, rbx: usize, rax: usize,
    // CPU-pushed iretq frame
    rip: usize, cs: usize, rflags: usize, rsp: usize, ss: usize,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct TaskContext {
    // Matches the 272-byte exception_vector_table layout
    regs: [usize; 31], // x0 through x30
    spsr: usize,       // Saved Program Status Register
    elr: usize,        // Exception Link Register (Entry point)
    _pad: usize,       // 16-byte alignment padding
}

/// A 16-byte aligned stack wrapper
#[repr(C, align(16))]
struct TaskStack([u8; STACK_SIZE]);

pub struct Task {
    pub id: usize,
    pub sp: usize, // Universal Stack Pointer naming
    pub state: TaskState,
    stack: TaskStack,
}

impl Task {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            sp: 0,
            state: TaskState::Terminated,
            stack: TaskStack([0; STACK_SIZE]),
        }
    }
    /// Voluntarily yield the remaining CPU timeslice to the next ready task.
pub fn yield_now() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("int 0x20", options(nomem, nostack));
    }

    #[cfg(target_arch = "aarch64")]
    unsafe {
        // Trigger a software interrupt yield trap
        core::arch::asm!("svc #0", options(nomem, nostack));
    }
}


    /// Initializes the stack frame and registers for a new task.
    pub fn init(&mut self, id: usize, entry: extern "C" fn() -> !) {
        self.id = id;
        self.state = TaskState::Ready;

        let stack_top = self.stack.0.as_ptr() as usize + STACK_SIZE;
        
        // Ensure 16-byte alignment mandated by both x86_64 ABI and ARM64 AAPCS
        let aligned_top = stack_top & !0xF;

        // Overlay our typed Context struct at the top of the new stack
        let frame_ptr = (aligned_top - core::mem::size_of::<TaskContext>()) as *mut TaskContext;

        #[cfg(target_arch = "x86_64")]
        unsafe {
            frame_ptr.write(TaskContext {
                r15: 0, r14: 0, r13: 0, r12: 0,
                r11: 0, r10: 0, r9: 0,  r8: 0,
                rbp: 0, rdi: 0, rsi: 0, rdx: 0,
                rcx: 0, rbx: 0, rax: 0,
                rip: entry as usize,
                cs: 0x08,              // Kernel Code Segment
                rflags: 0x202,         // Interrupts Enabled (IF bit 9)
                rsp: stack_top,
                ss: 0x10,              // Kernel Data Segment
            });
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            frame_ptr.write(TaskContext {
                regs: [0; 31],
                spsr: 0x05,            // EL1h mode with interrupts unmasked
                elr: entry as usize,   // Entry point execution
                _pad: 0,
            });
        }

        self.sp = frame_ptr as usize;
    }
}

// ==========================================
// Kernel Scheduler State
// ==========================================

static mut TASKS: [Task; MAX_TASKS] = [
    Task::empty(), Task::empty(), Task::empty(), Task::empty(),
];

static CURRENT_TASK: AtomicUsize = AtomicUsize::new(0);
static TASK_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Prepares a new task with its own stack and initial entry point function.
pub fn spawn(entry_point: extern "C" fn() -> !) -> bool {
    unsafe {
        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                TASKS[i].init(i, entry_point);

                // If this is the first task spawned, lock the current thread 
                // (main kernel loop) as Task 0 so it isn't orphaned.
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

/// The core scheduling logic called by interrupts.rs on every timer tick.
#[unsafe(no_mangle)]
pub extern "C" fn run_schedule(current_sp: usize) -> usize {
    if !TASK_INITIALIZED.load(Ordering::Relaxed) {
        return current_sp;
    }

    unsafe {
        let current_idx = CURRENT_TASK.load(Ordering::Relaxed);

        // Save current task's state if it is still running
        if current_sp != 0 && TASKS[current_idx].state == TaskState::Running {
            TASKS[current_idx].sp = current_sp;
            TASKS[current_idx].state = TaskState::Ready;
        }

        // Simple Round-Robin: Find the next Ready task
        let mut next_idx = current_idx;
        for _ in 0..MAX_TASKS {
            next_idx = (next_idx + 1) % MAX_TASKS;
            if TASKS[next_idx].state == TaskState::Ready {
                TASKS[next_idx].state = TaskState::Running;
                CURRENT_TASK.store(next_idx, Ordering::Relaxed);
                
                return TASKS[next_idx].sp;
            }
        }

        // Fallback: Stick to the current task if no other tasks are ready
        if TASKS[current_idx].state == TaskState::Ready {
            TASKS[current_idx].state = TaskState::Running;
        }
        
        current_sp
    }
}
