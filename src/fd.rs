//! POSIX Standard File Descriptors for mitosOS.

pub trait FileDescriptor {
    fn read(&mut self, buf: &mut [u8]) -> usize;
    fn write(&mut self, buf: &[u8]) -> usize;
}

pub struct UartFd;

impl FileDescriptor for UartFd {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() { return 0; }
        // Pull bytes from your interrupt input buffer
        if let Some(b) = crate::interrupts::dequeue_byte() {
            buf[0] = b;
            1
        } else {
            0
        }
    }

    fn write(&mut self, buf: &[u8]) -> usize {
        let mut uart = unsafe { crate::uart::Uart::init() };
        for &b in buf {
            let _ = core::fmt::Write::write_char(&mut uart, b as char);
        }
        buf.len()
    }
}
