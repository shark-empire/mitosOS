//! Minimal, hand-written PL011 UART driver (UART0) for the BCM2837
//! peripheral set used by the Raspberry Pi 3 — and emulated identically
//! by QEMU's `-M raspi3b`, so this works unchanged in both places.
//!
//! Written from scratch instead of pulling in a third-party UART crate:
//! fewer dependencies means a smaller, more auditable attack surface.

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

/// # Safety
/// `reg` must be a valid, correctly-aligned MMIO register address.
unsafe fn write_reg(reg: *mut u32, value: u32) {
    unsafe { core::ptr::write_volatile(reg, value) };
}

/// # Safety
/// `reg` must be a valid, correctly-aligned MMIO register address.
unsafe fn read_reg(reg: *mut u32) -> u32 {
    unsafe { core::ptr::read_volatile(reg) }
}

/// Crude busy-wait. Precise timing doesn't matter here — the GPIO
/// pull-up/down handshake below just needs "a couple hundred cycles".
fn delay(cycles: u32) {
    for _ in 0..cycles {
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
}

pub struct Uart;

impl Uart {
    /// Route GPIO14/15 to UART0 (PL011) and bring it up at 115200 8N1.
    ///
    /// # Safety
    /// Must only be called once, from core 0, before anything else
    /// touches these MMIO registers — true at the top of `kernel_main`.
    pub unsafe fn init() -> Self {
        // The whole bring-up sequence is one unsafe unit: see the # Safety
        // note above for why it's sound as a whole.
        unsafe {
            // Disable UART0 while we reconfigure it.
            write_reg(UART0_CR, 0);

            // Route GPIO14 (TXD0) and GPIO15 (RXD0) to ALT0, the PL011 function.
            let mut sel = read_reg(GPFSEL1);
            sel &= !((0b111 << 12) | (0b111 << 15));
            sel |= (0b100 << 12) | (0b100 << 15);
            write_reg(GPFSEL1, sel);

            // Disable pull-up/down on those two pins (BCM2837's documented
            // handshake: write control, wait, clock it in, wait, clear clock).
            write_reg(GPPUD, 0);
            delay(150);
            write_reg(GPPUDCLK0, (1 << 14) | (1 << 15));
            delay(150);
            write_reg(GPPUDCLK0, 0);

            // Clear any pending interrupts.
            write_reg(UART0_ICR, 0x7FF);

            // 115200 baud from the default 3 MHz UART clock:
            // divisor = 3_000_000 / (16 * 115200) = 1.627 -> IBRD=1, FBRD=40.
            write_reg(UART0_IBRD, 1);
            write_reg(UART0_FBRD, 40);

            // 8 bits, no parity, one stop bit, FIFOs enabled.
            write_reg(UART0_LCRH, (1 << 4) | (1 << 5) | (1 << 6));

            // Enable UART, transmit, and receive.
            write_reg(UART0_CR, (1 << 0) | (1 << 8) | (1 << 9));
        }

        Uart
    }

    pub fn write_byte(&mut self, byte: u8) {
        // FR bit 5: transmit FIFO full. Wait until there's room.
        while unsafe { read_reg(UART0_FR) } & (1 << 5) != 0 {}
        unsafe { write_reg(UART0_DR, byte as u32) };
    }

    /// Block until a byte arrives, then return it.
    pub fn read_byte(&mut self) -> u8 {
        // FR bit 4: receive FIFO empty. Wait until there's data.
        while unsafe { read_reg(UART0_FR) } & (1 << 4) != 0 {}
        unsafe { read_reg(UART0_DR) as u8 }
    }
}

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
