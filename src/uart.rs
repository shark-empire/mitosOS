//! UART driver, with separate backends per target architecture:
//! PL011 MMIO for the Raspberry Pi (aarch64), 16550 port I/O for
//! BIOS/QEMU x86_64. Both expose the same `Uart` API so call sites
//! in main.rs/shell.rs don't need per-arch branching.
//!
//! `read_byte`/`write_byte` still poll hardware and block until
//! ready — unchanged from before. What's new: receive errors
//! (framing/parity/break/overrun) are detected and the bad byte is
//! discarded instead of silently corrupting the caller's data, and
//! a bounded `try_read_byte` is available for callers that need a
//! timeout instead of blocking forever.

const POLL_ATTEMPTS: u32 = 1_000_000;

/// Errors a receive can report. `read_byte` handles these
/// internally by retrying; `try_read_byte` surfaces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartError {
    /// No byte arrived within `POLL_ATTEMPTS` iterations.
    Timeout,
    /// A byte arrived but failed a framing/parity/break/overrun
    /// check and was discarded.
    LineError,
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use super::{UartError, POLL_ATTEMPTS};

    const PERIPHERAL_BASE: usize = 0x3F00_0000;
    const GPIO_BASE: usize = PERIPHERAL_BASE + 0x20_0000;
    const UART0_BASE: usize = PERIPHERAL_BASE + 0x20_1000;

    const GPFSEL1: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
    const GPPUD: *mut u32 = (GPIO_BASE + 0x94) as *mut u32;
    const GPPUDCLK0: *mut u32 = (GPIO_BASE + 0x98) as *mut u32;

    const UART0_DR: *mut u32 = UART0_BASE as *mut u32;
    const UART0_RSRECR: *mut u32 = (UART0_BASE + 0x04) as *mut u32;
    const UART0_FR: *mut u32 = (UART0_BASE + 0x18) as *mut u32;
    const UART0_IBRD: *mut u32 = (UART0_BASE + 0x24) as *mut u32;
    const UART0_FBRD: *mut u32 = (UART0_BASE + 0x28) as *mut u32;
    const UART0_LCRH: *mut u32 = (UART0_BASE + 0x2C) as *mut u32;
    const UART0_CR: *mut u32 = (UART0_BASE + 0x30) as *mut u32;
    const UART0_IMSC: *mut u32 = (UART0_BASE + 0x38) as *mut u32;
    const UART0_ICR: *mut u32 = (UART0_BASE + 0x44) as *mut u32;

    const FR_RXFE: u32 = 1 << 4;
    const FR_TXFF: u32 = 1 << 5;
    const RSRECR_ANY_ERROR: u32 = 0b1111; // FE|PE|BE|OE, bits 0-3

    unsafe fn write_reg(reg: *mut u32, value: u32) {
        unsafe { core::ptr::write_volatile(reg, value) };
    }
    
    unsafe fn read_reg(reg: *mut u32) -> u32 {
        unsafe { core::ptr::read_volatile(reg) }
    }
    
    fn delay(cycles: u32) {
        for _ in 0..cycles {
            unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
        }
    }

    pub struct Uart;

    impl Uart {
        /// # Safety
        /// Must only be called once, from core 0, before anything else
        /// touches these MMIO registers — true at the top of `kernel_main`.
        pub unsafe fn init() -> Self {
            unsafe {
                write_reg(UART0_CR, 0);

                let mut sel = read_reg(GPFSEL1);
                sel &= !((0b111 << 12) | (0b111 << 15));
                sel |= (0b100 << 12) | (0b100 << 15);
                write_reg(GPFSEL1, sel);

                write_reg(GPPUD, 0);
                delay(150);
                write_reg(GPPUDCLK0, (1 << 14) | (1 << 15));
                delay(150);
                write_reg(GPPUDCLK0, 0);

                write_reg(UART0_ICR, 0x7FF);
                write_reg(UART0_IBRD, 1);
                write_reg(UART0_FBRD, 40);
                write_reg(UART0_LCRH, (1 << 4) | (1 << 5) | (1 << 6));
                write_reg(UART0_CR, (1 << 0) | (1 << 8) | (1 << 9));
            }
            Uart
        }

        /// Unmasks RXIM (FIFO crossed trigger level) and RTIM
        /// (timeout — fires even for one byte that never fills the
        /// FIFO). Both are needed or a lone keystroke can sit
        /// unreported.
        ///
        /// # Safety
        /// This only unmasks the UART's own interrupt source. The
        /// GIC isn't configured to route this IRQ to a core, there's
        /// no vector table entry for it, and `PSTATE.I` is still
        /// masked everywhere. Calling this today is inert — the
        /// interrupt has nowhere to go — but calling it after a
        /// *partial*, wrongly-ordered setup elsewhere could trap
        /// into an undefined vector. `unsafe` reflects that real risk.
        pub unsafe fn enable_interrupts(&mut self) {
            unsafe {
                write_reg(UART0_ICR, 0x7FF);
                let current = read_reg(UART0_IMSC);
                write_reg(UART0_IMSC, current | (1 << 4) | (1 << 6));
            }
        }

        pub fn write_byte(&mut self, byte: u8) {
            while unsafe { read_reg(UART0_FR) } & FR_TXFF != 0 {}
            unsafe { write_reg(UART0_DR, byte as u32) };
        }

        /// Blocks until a valid byte arrives; bad bytes are dropped
        /// and waited past rather than returned.
        pub fn read_byte(&mut self) -> u8 {
            loop {
                if let Ok(byte) = self.try_read_byte() {
                    return byte;
                }
            }
        }

        pub fn try_read_byte(&mut self) -> Result<u8, UartError> {
            for _ in 0..POLL_ATTEMPTS {
                if unsafe { read_reg(UART0_FR) } & FR_RXFE != 0 {
                    continue;
                }
                
                let byte = unsafe { read_reg(UART0_DR) };
                let status = unsafe { read_reg(UART0_RSRECR) };
                
                if status & RSRECR_ANY_ERROR != 0 {
                    unsafe { write_reg(UART0_RSRECR, 0) }; // any write clears
                    return Err(UartError::LineError);
                }
                
                return Ok(byte as u8);
            }
            Err(UartError::Timeout)
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use super::{UartError, POLL_ATTEMPTS};

    const COM1: u16 = 0x3F8;
    const LSR_DATA_READY: u8 = 1 << 0;
    const LSR_THR_EMPTY: u8 = 1 << 5;
    const LSR_ANY_ERROR: u8 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4); // OE|PE|FE|BI

    unsafe fn outb(port: u16, value: u8) {
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") port,
                in("al") value,
                options(nomem, nostack, preserves_flags)
            );
        }
    }
    
    unsafe fn inb(port: u16) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!(
                "in al, dx",
                out("al") value,
                in("dx") port,
                options(nomem, nostack, preserves_flags)
            );
        }
        value
    }

    pub struct Uart;

    impl Uart {
        /// # Safety
        /// Must only be called once, before anything else touches port
        /// 0x3F8 — true at the top of `kernel_main`.
        pub unsafe fn init() -> Self {
            unsafe {
                outb(COM1 + 1, 0x00);
                outb(COM1 + 3, 0x80);
                outb(COM1 + 0, 0x03);
                outb(COM1 + 1, 0x00);
                outb(COM1 + 3, 0x03);
                outb(COM1 + 2, 0xC7);
                outb(COM1 + 4, 0x0B);
            }
            Uart
        }

        /// # Safety
        /// This only unmasks the UART's own interrupt source. There's
        /// no IDT entry for this vector, the PIC hasn't been remapped
        /// or told to unmask IRQ4, and `RFLAGS.IF` is still clear.
        /// Calling this today is inert; calling it after a partial,
        /// wrongly-ordered setup elsewhere is genuinely unsafe.
        pub unsafe fn enable_interrupts(&mut self) {
            unsafe {
                let current = inb(COM1 + 1);
                outb(COM1 + 1, current | 0x01);
            }
        }

        pub fn write_byte(&mut self, byte: u8) {
            while unsafe { inb(COM1 + 5) } & LSR_THR_EMPTY == 0 {}
            unsafe { outb(COM1, byte) };
        }

        pub fn read_byte(&mut self) -> u8 {
            loop {
                if let Ok(byte) = self.try_read_byte() {
                    return byte;
                }
            }
        }

        pub fn try_read_byte(&mut self) -> Result<u8, UartError> {
            for _ in 0..POLL_ATTEMPTS {
                let status = unsafe { inb(COM1 + 5) };
                
                if status & LSR_DATA_READY == 0 {
                    continue;
                }
                
                if status & LSR_ANY_ERROR != 0 {
                    unsafe { inb(COM1) }; // consume the bad byte, clears DR
                    return Err(UartError::LineError);
                }
                
                return Ok(unsafe { inb(COM1) });
            }
            Err(UartError::Timeout)
        }
    }
}

pub use imp::Uart;

impl core::fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}
