//! Process-facing API surface for mitosOS.
//!
//! This is deliberately thin. Every piece of what a naive version of
//! this module would do -- allocate a page table, load the ELF,
//! allocate a user stack, drop privilege to ring 3/EL0 -- already
//! exists in `task::spawn_from_elf`, wired into the scheduler,
//! ring-3-isolated, and covered by CI on both architectures. A
//! separate implementation here would either have to duplicate all of
//! that (drifting out of sync with it over time, the way this file's
//! first draft already had a stale/incorrect stack allocator and an
//! `enter_user_mode` that bypassed the scheduler entirely by
//! `iretq`/`eret`-ing directly out of whatever called it -- permanently,
//! since neither instruction returns), or just call through to it.
//! This does the latter.

/// Unique identifier for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub usize);

/// Execution state of a process, mirroring `task::TaskState`.
///
/// Kept as a separate, smaller public enum rather than re-exporting
/// `task::TaskState` directly: callers of this module's API shouldn't
/// need to know about scheduler-internal states (a task table slot
/// being `Ready` vs `Running` is an implementation detail), just
/// whether the process they spawned is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Alive,
    Exited,
}

/// Spawns an ELF binary as a new, ring-3-isolated process and hands it
/// off to the scheduler.
///
/// Unlike an earlier version of this function, this returns as soon
/// as the process is registered -- it does *not* drop privilege and
/// jump to it directly. Doing that from here would mean never
/// returning to the caller at all (the whole point of `iretq`/`eret`
/// is a one-way trip), which for a call made during boot would mean
/// permanently replacing `kmain` with this one process: no scheduler,
/// no shell, no background tasks, ever. Spawning through the task
/// table instead means the new process starts running the next time
/// the scheduler picks it, same as anything spawned from the shell's
/// `run` command.
pub fn spawn_and_run_elf(elf_binary: &[u8]) -> Result<(), &'static str> {
    if crate::task::spawn_from_elf(elf_binary) {
        Ok(())
    } else {
        Err("spawn_from_elf failed (bad ELF, allocation failure, or no free task slot)")
    }
}
