// kernel/src/task.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Terminated,
}

pub struct Task {
    pub id: usize,
    pub rsp: usize, // Saved Stack Pointer
    pub state: TaskState,
    // Each task gets a dedicated 4KB stack in BSS memory
    stack: [u8; 4096], 
}

impl Task {
    const fn empty() -> Self {
        Self {
            id: 0,
            rsp: 0,
            state: TaskState::Terminated,
            stack: [0; 4096],
        }
    }
}

// Max 4 concurrent tasks for now

const MAX_TASKS: usize = 4;
static mut TASKS: [Task; MAX_TASKS] = [const { Task::empty() }; MAX_TASKS];

static mut CURRENT_TASK: usize = 0;
static mut TASK_INITIALIZED: bool = false;

#[derive(Clone, Copy)]
/// Prepares a new task with its own stack and initial entry point function.
pub unsafe fn spawn(entry_point: extern "C" fn() -> !) -> bool {
    unsafe {
        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                let task = &mut TASKS[i];
                task.id = i;
                task.state = TaskState::Ready;

                // Point to the top of the 4KB stack (stacks grow downwards in x86_64)
                let stack_top = task.stack.as_ptr() as usize + task.stack.len();
                
                // Align stack to 16 bytes as required by the x86_64 ABI
                let mut sp = stack_top & !0xF;

                // We simulate what the CPU push sequence looks like during an interrupt,
                // so when `timer_handler_stub` executes `iretq`, it seamlessly jumps into 
                // our new task's entry point!
                
                // 1. Alignment dummy or padding if needed, then fake register pushes 
                // matching the exact order in `timer_handler_stub`:
                // r15, r14, r13, r12, r11, r10, r9, r8, rbp, rdi, rsi, rdx, rcx, rbx, rax
                for _ in 0..15 {
                    sp -= 8;
                    *(sp as *mut usize) = 0;
                }

                // Push fake stack frame for iretq: RIP, CS, RFLAGS, RSP, SS
                sp -= 8; *(sp as *mut usize) = 0x10; // SS (Data Segment)
                sp -= 8; *(sp as *mut usize) = sp + 32; // RSP placeholder
                sp -= 8; *(sp as *mut usize) = 0x202; // RFLAGS (Interrupts enabled bit set)
                sp -= 8; *(sp as *mut usize) = 0x08; // CS (Code Segment)
                sp -= 8; *(sp as *mut usize) = entry_point as usize; // RIP (Function entry)

                task.rsp = sp;
                
                if !TASK_INITIALIZED {
                    // Task 0 is whatever is currently running (the main kernel/shell)
                    TASKS[0].state = TaskState::Running;
                    TASK_INITIALIZED = true;
                }
                return true;
            }
        }
    }
    false
}

/// The core scheduling logic called by interrupts.rs on every timer tick.
#[unsafe(no_mangle)]
pub extern "C" fn schedule(current_rsp: usize) -> usize {
    unsafe {
        if !TASK_INITIALIZED {
            return current_rsp;
        }

        // Save current task's stack pointer
        TASKS[CURRENT_TASK].rsp = current_rsp;

        // Simple Round-Robin: Find the next Ready task
        let mut next_task = CURRENT_TASK;
        for _ in 0..MAX_TASKS {
            next_task = (next_task + 1) % MAX_TASKS;
            if TASKS[next_task].state == TaskState::Ready {
                break;
            }
        }

        if TASKS[next_task].state == TaskState::Ready {
            TASKS[CURRENT_TASK].state = TaskState::Ready;
            CURRENT_TASK = next_task;
            TASKS[CURRENT_TASK].state = TaskState::Running;
            return TASKS[CURRENT_TASK].rsp;
        }

        // Fallback: stick to current task if no other task is ready
        current_rsp
    }
}
