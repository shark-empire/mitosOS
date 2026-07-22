//! Professional Production-Ready POSIX File Descriptor Subsystem for mitosOS.

use alloc::vec::Vec;
use alloc::boxed::Box;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdError {
    WouldBlock,
    BadRequest,
    IoError,
    NoSpace,
}

pub trait FileDescriptor: Send + Sync {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FdError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, FdError>;
    fn flush(&mut self) -> Result<(), FdError> {
        Ok(())
    }
}

pub struct UartFd;

impl UartFd {
    pub const fn new() -> Self {
        Self
    }
}

impl FileDescriptor for UartFd {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FdError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            if let Some(b) = crate::interrupts::dequeue_byte() {
                buf[bytes_read] = b;
                bytes_read += 1;
            } else {
                break;
            }
        }

        if bytes_read == 0 {
            Err(FdError::WouldBlock)
        } else {
            Ok(bytes_read)
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, FdError> {
        let mut uart = unsafe { crate::uart::Uart::init() };
        for &b in buf {
            let _ = core::fmt::Write::write_char(&mut uart, b as char);
        }
        Ok(buf.len())
    }
}

const MAX_PROCESS_FDS: usize = 64;

pub struct FileDescriptorTable {
    fds: Vec<Option<Box<dyn FileDescriptor>>>,
}

impl FileDescriptorTable {
    pub fn new() -> Self {
        let mut table = Self {
            fds: Vec::with_capacity(MAX_PROCESS_FDS),
        };

        table.fds.push(Some(Box::new(UartFd::new())));
        table.fds.push(Some(Box::new(UartFd::new())));
        table.fds.push(Some(Box::new(UartFd::new())));

        table
    }

    pub fn allocate(&mut self, fd: Box<dyn FileDescriptor>) -> Result<usize, FdError> {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(fd);
                return Ok(i);
            }
        }

        if self.fds.len() < MAX_PROCESS_FDS {
            let id = self.fds.len();
            self.fds.push(Some(fd));
            return Ok(id);
        }

        Err(FdError::NoSpace)
    }

    pub fn get(&mut self, fd: usize) -> Result<&mut dyn FileDescriptor + '_, FdError> {
        self.fds
            .get_mut(fd)
            .and_then(|slot| slot.as_mut().map(|boxed| boxed.as_mut()))
            .ok_or(FdError::BadRequest)
    }

    pub fn close(&mut self, fd: usize) -> Result<(), FdError> {
        if fd < self.fds.len() {
            if self.fds[fd].take().is_some() {
                return Ok(());
            }
        }
        Err(FdError::BadRequest)
    }
}
