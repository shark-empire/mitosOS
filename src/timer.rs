//! Hardware Timer Initialization for preemptive multitasking.

/// The target frequency for the scheduler tick (e.g., 100 Hz = 10ms per tick)
pub const TIMER_HZ: usize = 100;

#[cfg(target_arch = "x86_64")]
pub mod hardware {
    use super::TIMER_HZ;
    use core::arch::asm;

    const PIT_FREQ: usize = 1_193_182;
    const COMMAND_PORT: u16 = 0x43;
    const CHANNEL_0_PORT: u16 = 0x40;

    /// Writes a byte to an x86 I/O port
    unsafe fn outb(port: u16, val: u8) {
      unsafe {
         asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
              }
    }

    /// Initializes the x86 Programmable Interval Timer (PIT)
    pub fn init() {
        let divisor = PIT_FREQ / TIMER_HZ;
        
        unsafe {
            // Command 0x36: Channel 0, Lobyte/Hibyte access, Mode 3 (Square Wave)
            outb(COMMAND_PORT, 0x36);
            
            // Send low byte of divisor
            outb(CHANNEL_0_PORT, (divisor & 0xFF) as u8);
            
            // Send high byte of divisor
            outb(CHANNEL_0_PORT, ((divisor >> 8) & 0xFF) as u8);
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub mod hardware {
    use super::TIMER_HZ;
    use core::arch::asm;

    /// Initializes the ARMv8 Generic Timer (EL0 Physical Timer)
    pub fn init() {
        unsafe {
            let mut frq: usize;
            // Read the system counter frequency
            asm!("mrs {}, cntfrq_el0", out(reg) frq, options(nomem, nostack));
            
            let tick_rate = frq / TIMER_HZ;

            // Set the timer countdown value
            asm!("msr cntp_tval_el0, {}", in(reg) tick_rate, options(nomem, nostack));

            // Enable the physical timer (bit 0 = ENABLE, bit 1 = IMASK (0 to unmask))
            asm!("msr cntp_ctl_el0, {}", in(reg) 1_usize, options(nomem, nostack));
        }
    }

    /// Resets the ARM timer countdown. 
    /// Must be called inside the AArch64 timer interrupt handler to acknowledge the tick!
    pub fn reset_timer() {
        unsafe {
            let mut frq: usize;
            asm!("mrs {}, cntfrq_el0", out(reg) frq, options(nomem, nostack));
            let tick_rate = frq / TIMER_HZ;
            asm!("msr cntp_tval_el0, {}", in(reg) tick_rate, options(nomem, nostack));
        }
    }
}
