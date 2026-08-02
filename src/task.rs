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
    /// SP_EL0 -- a single global register, not banked per-task the way
    /// SP_EL1 effectively is (see the note on kernel_stack_top below).
    /// Every trap taken from EL0 must save/restore this explicitly
    /// (interrupts.rs's "Lower EL" vectors do) or a preempted EL0
    /// task would resume on whichever *other* task's user stack
    /// happened to be current last. This used to be unused padding,
    /// back when nothing ever actually reached EL0 -- same 8 bytes,
    /// same 272-byte frame size, just given a job.
    pub sp_el0: usize,
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
    /// True only when this task exclusively owns `memory_root` (a
    /// fresh frame from `memory::create_process_page_table`) and is
    /// therefore the one responsible for freeing it on exit. False for
    /// a `SharedThread` (whose root is the caller's live table --
    /// possibly the kernel's own boot root) and for an
    /// `IsolatedProcess` that fell back to sharing its parent's table
    /// after a page-table allocation failure (see
    /// `allocate_isolated_page_table`) -- neither actually owns that
    /// table, so freeing it on exit would corrupt memory something
    /// else is still using. Set once in `init`, consumed once by
    /// `run_schedule`'s exit-time cleanup.
    pub owns_memory_root: bool,
    /// True if this task runs in ring 3 / EL0. Lets the syscall layer
    /// (`syscall::validate_user_ptr`) tell a genuine userspace caller,
    /// whose pointers must be checked against its own page table,
    /// apart from a SharedThread kernel-mode caller (e.g. the shell),
    /// whose pointers are its own plain kernel-address locals and were
    /// never meant to be validated as "user" memory.
    pub is_ring3: bool,
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
            owns_memory_root: false,
            is_ring3: false,
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
    /// `memory_root` is installed as-is -- this does *not* allocate or
    /// clone a page table itself. Callers decide what root a task gets:
    /// `spawn` derives one (sharing the caller's for `SharedThread`,
    /// cloning a fresh one for a generic `IsolatedProcess`); `spawn_from_elf`
    /// (via `spawn_isolated_at`) passes in a table it already built and
    /// mapped the ELF's segments and stack into. `init` doesn't get a say
    /// in that decision -- if it derived its own here, callers who need an
    /// *already-populated* table (like `spawn_from_elf`) would have theirs
    /// silently discarded in favor of an empty one, and this task's entry
    /// point would page-fault the instant it tried to fetch its first
    /// instruction.
    ///
    /// `user_stack_top` is only meaningful for `ExecutionMode::IsolatedProcess`
    /// -- pass `0` for `SharedThread` (kernel-mode tasks don't have one).
    pub fn init(
        &mut self, 
        id: usize, 
        entry: extern "C" fn() -> !, 
        mode: ExecutionMode, 
        memory_root: usize,
        owns_memory_root: bool,
        user_stack_top: usize,
    ) {
        self.id = id;
        self.parent_id = if mode == ExecutionMode::SharedThread { id } else { id };
        self.state = TaskState::Ready;
        self.fd_table = Some(crate::fd::FileDescriptorTable::new()); 
        self.memory_root = memory_root;
        self.owns_memory_root = owns_memory_root;

        // Shared by both arch branches below -- previously computed
        // twice (once per `#[cfg]` block) with identical logic.
        let is_user = mode == ExecutionMode::IsolatedProcess && user_stack_top != 0;
        self.is_ring3 = is_user;

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
            // SPSR_EL1.M[3:0]: 0x0 = EL0t (the only valid EL0 mode --
            // there's no "EL0h"), 0x5 = EL1h. Leaving DAIF clear either
            // way means interrupts are unmasked the instant this task's
            // context is restored, matching x86_64's rflags=0x202 (IF=1).
            //
            // The user stack lives in SP_EL0, a completely separate
            // banked register from SP_EL1/`stack` below -- eret with
            // SPSR.M=EL0t switches to it automatically, but only once
            // interrupts.rs's "Lower EL" vectors have actually restored
            // it from here first.
            frame_ptr.write(TaskContext {
                regs: [0; 31],
                spsr: if is_user { 0x0 } else { 0x5 },
                elr: entry as usize,
                sp_el0: if is_user { user_stack_top } else { 0 },
            });
        }

        self.sp = frame_ptr as usize;
    }

    /// Top of this task's kernel stack -- the value TSS.RSP0 should hold
    /// while this task is the one running (see run_schedule).
    ///
    /// AArch64 has no equivalent of this function, deliberately -- not
    /// an oversight. x86_64 needs TSS.RSP0 because ring-3->ring-0 is a
    /// *single*, unbanked RSP register: the CPU has no memory of "what
    /// RSP this task's kernel side was using last time", so hardware
    /// has to be told explicitly, every switch, via a separate staging
    /// field, or an unrelated task's stale RSP gets reused.
    ///
    /// AArch64 doesn't have that problem: SP_EL1 is a banked register,
    /// physically separate from SP_EL0, so it simply *stays* wherever
    /// EL1 code last left it -- including across an eret to EL0 and
    /// back. interrupts.rs's exception stubs already do `mov sp, x0`
    /// with a pointer into *this specific task's* `stack` buffer as
    /// part of every restore (needed regardless, to reach the saved
    /// register values), which means SP_EL1 is left pointing at that
    /// same task-private buffer the moment we eret away -- exactly
    /// what a per-task kernel landing stack needs, with no additional
    /// staging step. `stack` already serves both roles (initial
    /// bootstrap frame *and* every later EL0->EL1 trap landing) with
    /// nothing extra required.
    #[cfg(target_arch = "x86_64")]
    fn kernel_stack_top(&self) -> usize {
        (self.kernel_stack.0.as_ptr() as usize + STACK_SIZE) & !0xF
    }
}



/// Gets the ID of the currently executing task.
pub fn current_task_id() -> usize {
    CURRENT_TASK.load(Ordering::Relaxed)
}

/// Returns `(memory_root, is_ring3)` for the currently scheduled task.
/// Used by the syscall layer to decide whether (and against which
/// table) a raw caller-supplied pointer needs validating -- see
/// `syscall::validate_user_ptr`.
pub fn current_task_access_info() -> (usize, bool) {
    unsafe {
        let idx = CURRENT_TASK.load(Ordering::Relaxed);
        (TASKS[idx].memory_root, TASKS[idx].is_ring3)
    }
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
        let caller_root = current_memory_root();
        let (memory_root, owns_memory_root) = match mode {
            ExecutionMode::SharedThread => (caller_root, false),
            ExecutionMode::IsolatedProcess => allocate_isolated_page_table(caller_root),
        };

        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                TASKS[i].init(i, entry_point, mode, memory_root, owns_memory_root, user_stack_top);

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

/// Registers a new `IsolatedProcess` task using a page table root the
/// caller has *already built and populated* -- unlike `spawn`/`spawn_at`,
/// this never calls `allocate_isolated_page_table` itself.
///
/// `spawn_from_elf` needs exactly this. It builds the process's page
/// table up front so `elf::load_elf_to_process` and `allocate_user_stack`
/// have somewhere real to map into -- before this task is even
/// registered, let alone running -- then has to hand *that exact table*
/// here. Going through `spawn`/`spawn_at` instead would derive a brand
/// new, empty table for `IsolatedProcess` (see `spawn`) and install that
/// one, leaving the real one -- the one with the ELF's segments and
/// stack already mapped into it -- orphaned. The task's entry point and
/// stack would both be valid-looking addresses that simply aren't
/// mapped in whatever table actually lands in CR3/TTBR0, so the very
/// first instruction fetch after the ring-3 transition page-faults.
/// (This is exactly what was happening before this function existed.)
fn spawn_isolated_at(entry_addr: usize, memory_root: usize, owns_memory_root: bool, user_stack_top: usize) -> bool {
    let entry_point: extern "C" fn() -> ! = unsafe { core::mem::transmute(entry_addr) };
    unsafe {
        for i in 0..MAX_TASKS {
            if TASKS[i].state == TaskState::Terminated {
                TASKS[i].init(i, entry_point, ExecutionMode::IsolatedProcess, memory_root, owns_memory_root, user_stack_top);

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


/// Allocates a new page table root for an isolated process. Returns
/// `(root, true)` when `root` is a fresh frame this process
/// exclusively owns. On allocation failure this falls back to sharing
/// `parent_root` so the caller keeps running instead of the spawn
/// failing outright -- `(parent_root, false)` tells callers this root
/// is *not* this task's to free (see `Task::owns_memory_root`), since
/// it's the parent's live table, not a private one.
fn allocate_isolated_page_table(parent_root: usize) -> (usize, bool) {
    unsafe {
        match crate::memory::create_process_page_table() {
            Some(root) => (root, true),
            None => (parent_root, false),
        }
    }
}

/// Maps a small stack for an isolated process's ring-3 code, in that
/// process's *own* page table -- user-accessible, writable, non-executable.
/// There wasn't one at all before this; every task ran on `stack` (plain
/// kernel memory, never user-accessible) regardless of mode. Returns the
/// *top* of the stack (stacks grow down towards lower addresses).
///
/// USER_SPACE_BASE + 0x1000_0000 (256MB into the private region) so
/// there's plenty of room below it for an ELF binary linked near the
/// start of that region, per memory::USER_SPACE_BASE's doc comment.
/// Every process uses this same address -- that's fine now, not a
/// collision: each process's copy of the top-level table has this
/// index unpopulated, so mapping here forces a fresh, private
/// PDPT/PD/PT chain per process rather than walking into a shared one.
#[cfg(target_arch = "x86_64")]
fn allocate_user_stack(page_table_root: usize) -> Option<usize> {
    const USER_STACK_TOP: usize = crate::memory::USER_SPACE_BASE + 0x1000_0000;
    const USER_STACK_PAGES: usize = 4; // 16KB
    const PAGE_SIZE: usize = 4096;

    let root = page_table_root as *mut crate::vmm::arch::PageTable;
    let flags = crate::memory::MapFlags {
        writable: true,
        user_accessible: true,
        execute_disable: true,
        device: false,
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

/// AArch64 equivalent of the x86_64 function above -- same address,
/// same page count, same reasoning, using vmm::arch::map_page's
/// AArch64 branch instead of the x86_64 one.
#[cfg(target_arch = "aarch64")]
fn allocate_user_stack(page_table_root: usize) -> Option<usize> {
    const USER_STACK_TOP: usize = crate::memory::USER_SPACE_BASE + 0x1000_0000;
    const USER_STACK_PAGES: usize = 4; // 16KB
    const PAGE_SIZE: usize = 4096;

    let root = page_table_root as *mut crate::vmm::arch::PageTable;
    let flags = crate::memory::MapFlags {
        writable: true,
        user_accessible: true,
        execute_disable: true,
        device: false,
    };

    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
    for i in 0..USER_STACK_PAGES {
        let vaddr = stack_bottom + i * PAGE_SIZE;
        let phys = crate::memory::vmm_alloc_frame()?;
        unsafe {
            core::ptr::write_bytes(phys as *mut u8, 0, PAGE_SIZE);
            crate::vmm::arch::map_page(root, vaddr, phys, flags).ok()?;
        }
    }

    Some(USER_STACK_TOP)
}

/// Spawns a new isolated process from an ELF binary in memory.
pub fn spawn_from_elf(elf_bytes: &[u8]) -> bool {
    let parent_root = current_memory_root();
    
    // 1. Create a new memory space for the process
    let (page_table_root, owns_memory_root) = allocate_isolated_page_table(parent_root);
    
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

    // 4. Register the task with *this exact* page table -- the one that
    //    steps 2 and 3 actually mapped the ELF's segments and stack
    //    into. See spawn_isolated_at for why this can't go through the
    //    ordinary spawn_at/spawn path.
    spawn_isolated_at(entry_point, page_table_root, owns_memory_root, user_stack_top)
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
                    // mov-to-CR3 flushes all non-global TLB entries as
                    // an architectural side effect -- nothing extra
                    // needed here for the same reason AArch64's branch
                    // below needs an explicit tlbi.
                    core::arch::asm!("mov cr3, {}", in(reg) next_root, options(nostack, preserves_flags));
                    
                    #[cfg(target_arch = "aarch64")]
                    // Unlike x86_64's mov-to-CR3, msr ttbr0_el1 does
                    // *not* implicitly flush anything -- without the
                    // tlbi here, a stale entry cached from whichever
                    // process last used this same virtual address
                    // (every process's stack sits at the same address,
                    // memory::USER_SPACE_BASE + 0x1000_0000, by design)
                    // could still resolve to the *previous* occupant's
                    // physical frame instead of faulting through to
                    // the new page table, silently handing one process
                    // read/write access to another's private memory.
                    // vmalle1 is a blunt, whole-TLB instrument -- ASID
                    // tagging would let this scope down to just the
                    // outgoing task's entries and skip the flush
                    // entirely on a cache hit, but that requires
                    // invalidating by ASID specifically when a task
                    // slot is *reused* by a new process (same task ID,
                    // different address space underneath), which has
                    // enough edge cases to get subtly wrong that it's
                    // not worth the risk without being able to test it.
                    // A full flush on every switch is the same cost
                    // x86_64 already unconditionally pays via CR3, so
                    // this isn't a regression relative to that, just
                    // an explicit version of what x86_64 gets for free.
                    core::arch::asm!(
                        "msr ttbr0_el1, {root}",
                        "isb",
                        "tlbi vmalle1",
                        "dsb ish",
                        "isb",
                        root = in(reg) next_root,
                        options(nostack, preserves_flags),
                    );
                }

                // --- Reclaim a terminated task's private memory ---
                // Safe exactly here, not in `exit()` itself: the
                // address-space switch above (if it ran) has already
                // moved CR3/TTBR0_EL1 off `current_idx`'s table, so
                // freeing its frames now can't pull memory out from
                // under a translation the CPU is still using. `exit()`
                // can't do this itself -- by the time it could, it
                // would be freeing its own currently-active address
                // space out from under itself, mid-instruction.
                //
                // `owns_memory_root` (see its doc comment on `Task`)
                // is what keeps this from ever freeing a table another
                // task still depends on: a SharedThread's root is the
                // caller's live table, and an IsolatedProcess that fell
                // back to sharing its parent's table after an
                // allocation failure doesn't own it either. Only a
                // root this exact task got exclusively is freed.
                //
                // Runs once per terminated task, not once per tick:
                // the round-robin search above only ever selects a
                // `Ready` task as `next_idx`, so `CURRENT_TASK` can
                // never point back at this now-Terminated slot again
                // on a later call -- not until a future `spawn()`
                // reuses it, which re-Readies it with a fresh
                // `owns_memory_root` for the new occupant first.
                if TASKS[current_idx].state == TaskState::Terminated
                    && TASKS[current_idx].owns_memory_root
                {
                    let dead_root = TASKS[current_idx].memory_root;
                    // Cleared before the (potentially long) free walk,
                    // not after -- belt-and-braces against this same
                    // branch somehow running twice for one slot.
                    TASKS[current_idx].owns_memory_root = false;
                    TASKS[current_idx].memory_root = 0;
                    crate::vmm::free_process_page_table(
                        dead_root as *mut crate::vmm::arch::PageTable,
                    );
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
