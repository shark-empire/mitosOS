//! Synchronization primitives for mitosOS.
//! Provides spinlocks compatible with bare-metal x86_64 and AArch64.

core::arch::global_asm!(""); // Placeholder if needed for asm constraints

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A mutual exclusion primitive that spins and disables interrupts
/// to safely share data between tasks and interrupt handlers.
pub struct Spinlock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Spinlock<T> {}
unsafe impl<T: Send> Send for Spinlock<T> {}

pub struct SpinlockGuard<'a, T> {
    lock: &'a Spinlock<T>,
    saved_interrupt_state: bool,
}

impl<T> Spinlock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinlockGuard<'_, T> {
        // Disable interrupts before spinning to prevent deadlock 
        // if an interrupt handler tries to acquire the same lock.
        let saved_interrupt_state = disable_and_save_interrupts();

        while self.locked.swap(true, Ordering::Acquire) {
            // Hint to the CPU that we are spinning in a busy-wait loop
            core::hint::spin_loop();
        }

        SpinlockGuard {
            lock: self,
            saved_interrupt_state,
        }
    }
}

impl<T> Deref for SpinlockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinlockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinlockGuard<'_, T> {
    fn drop(&mut self) {
        // Release the lock
        self.lock.locked.store(false, Ordering::Release);
        
        // Restore previous CPU interrupt state
        if self.saved_interrupt_state {
           unsafe{
               crate::interrupts::enable_cpu_interrupts();
                 }
        }
    }
}

/// Helper to check and clear/save interrupt states per architecture
#[inline(always)]
fn disable_and_save_interrupts() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let flags: usize;
        unsafe {
            core::arch::asm!(
                "pushfq",
                "pop {0}",
                "cli",
                out(reg) flags,
                options(nomem, nostack)
            );
        }
        // Bit 9 (IF - Interrupt Flag) indicates if interrupts were enabled
        (flags & (1 << 9)) != 0
    }

    #[cfg(target_arch = "aarch64")]
    {
        let daif: usize;
        unsafe {
            core::arch::asm!(
                "mrs {0}, daif",
                "msr daifset, #2", // Mask IRQs
                out(reg) daif,
                options(nomem, nostack)
            );
        }
        // Bit 7 of DAIF indicates I (IRQ) mask bit. If 0, IRQs were enabled.
        (daif & (1 << 7)) == 0
    }
}
