//! UART driver, with separate backends per target architecture:
//! PL011 MMIO for the Raspberry Pi (aarch64), 16550 port I/O for
//! BIOS/QEMU x86_64. Both expose the same `Uart` API so call sites
//! in main.rs don't need per-arch branching.

#[cfg(target_arch = "aarch64")]
mod imp {
    const PERIPHERAL_BASE: usize = 0x3F00_0000;
    const GPIO_BASE: usize = PERIPHERAL_BASE + 0x20_0000;
    const UART0_BASE: usize = PERIPHERAL_BASE + 0x20_1000;

    const GPFSEL1: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
    const GPPUD: *mut u32 = (GPIO_BASE + 0x94) as *mut u32;
    const GPPUDCLK0: *mut u32 = (GPIO_BASE + 0x98) as *mut u32;

    const UART0_DR: *mut u32 = (UART0_BASE) as *mut u32;
    const UART0_FR: *mut u32 = (UART0_BASE + 0x18) as *mut u32;
    const UART0_IBRD: *mut u32 = (UART0_BASE + 0x24) as *mut u32;
    const UART0_FBRD: *mut u32 = (UART0_BASE + 0x28) as *mut u32;
    const UART0_LCRH: *mut u32 = (UART0_BASE + 0x2C) as *mut u32;
    const UART0_CR: *mut u32 = (UART0_BASE + 0x30) as *mut u32;
    const UART0_ICR: *mut u32 = (UART0_BASE + 0x44) as *mut u32;

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

        pub fn write_byte(&mut self, byte: u8) {
            while unsafe { read_reg(UART0_FR) } & (1 << 5) != 0 {}
            unsafe { write_reg(UART0_DR, byte as u32) };
        }

        pub fn read_byte(&mut self) -> u8 {
            while unsafe { read_reg(UART0_FR) } & (1 << 4) != 0 {}
            unsafe { read_reg(UART0_DR) as u8 }
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    //! Minimal, hand-written 16550 UART driver over COM1 (port 0x3F8),
    //! the serial port QEMU's `-serial stdio` exposes on `q35`. Port I/O
    //! rather than MMIO, so it needs no page-table entry to work.

    const COM1: u16 = 0x3F8;

    unsafe fn outb(port: u16, value: u8) {
        unsafe {
            core::arch::asm!(
                "out dx, al", in("dx") port, in("al") value,
                options(nomem, nostack, preserves_flags)
            );
        }
    }
    unsafe fn inb(port: u16) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!(
                "in al, dx", out("al") value, in("dx") port,
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

        pub fn write_byte(&mut self, byte: u8) {
            while unsafe { inb(COM1 + 5) } & 0x20 == 0 {}
            unsafe { outb(COM1, byte) };
        }

        pub fn read_byte(&mut self) -> u8 {
            while unsafe { inb(COM1 + 5) } & 0x01 == 0 {}
            unsafe { inb(COM1) }
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
