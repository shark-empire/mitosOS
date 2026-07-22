//! Professional Production-Ready POSIX File Descriptor Subsystem for mitosOS.

use alloc::vec::Vec;
use alloc::boxed::Box;

/// Standard I/O Error types for file descriptor operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdError {
    /// Operation would block (non-blocking I/O).
    WouldBlock,
    /// Invalid file descriptor or out of bounds.
    BadRequest,
    /// Underlying hardware or driver I/O error.
    IoError,
    /// Buffer is full or resource is exhausted.
    NoSpace,
}

/// POSIX-compliant File Descriptor abstraction trait.
pub trait FileDescriptor: Send + Sync {
    /// Reads bytes from the file descriptor into the provided buffer.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FdError>;

    /// Writes bytes from the provided buffer into the file descriptor.
    fn write(&mut self, buf: &[u8]) -> Result<usize, FdError>;

    /// Flushes any buffered data (optional override).
    fn flush(&mut self) -> Result<(), FdError> {
        Ok(())
    }
}

// ==========================================
// UART / Console File Descriptors
// ==========================================

/// Standard UART Serial File Descriptor.
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
        // Pull bytes from the asynchronous interrupt input buffer
        while bytes_read < buf.len() {
            if let Some(b) = crate::interrupts::dequeue_byte() {
                buf[bytes_read] = b;
                bytes_read += 1;
            } else {
                break; // Stop when input queue is empty
            }
        }

        if bytes_read == 0 {
            // If no data is available immediately, return WouldBlock or 0 depending on semantics
            Err(FdError::WouldBlock)
        } else {
            Ok(bytes_read)
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, FdError> {
        // Optimized: Writes directly to the hardware/global UART driver without re-initializing.
        for &b in buf {
            crate::uart::write_byte(b);
        }
        Ok(buf.len())
    }
}

// ==========================================
// Process File Descriptor Table
// ==========================================

/// Maximum number of open file descriptors per task/process.
const MAX_PROCESS_FDS: usize = 64;

/// Manages open file descriptors for an individual process.
pub struct FileDescriptorTable {
    fds: Vec<Option<Box<dyn FileDescriptor>>>,
}

impl FileDescriptorTable {
    /// Creates a new FD table initialized with standard streams (stdin, stdout, stderr).
    pub fn new() -> Self {
        let mut table = Self {
            fds: Vec::with_capacity(MAX_PROCESS_FDS),
        };

        // FD 0: Standard Input (UART)
        table.fds.push(Some(Box::new(UartFd::new())));
        // FD 1: Standard Output (UART)
        table.fds.push(Some(Box::new(UartFd::new())));
        // FD 2: Standard Error (UART)
        table.fds.push(Some(Box::new(UartFd::new())));

        table
    }

    /// Allocates a new file descriptor and returns its index (FD number).
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

    /// Retrieves a mutable reference to a file descriptor by its ID.
    pub fn get(&mut self, fd: usize) -> Result<&mut dyn FileDescriptor, FdError> {
        self.fds
            .get_mut(fd)
            .and_then(|slot| slot.as_mut().map(|boxed| boxed.as_mut()))
            .ok_or(FdError::BadRequest)
    }

    /// Closes and removes a file descriptor.
    pub fn close(&mut self, fd: usize) -> Result<(), FdError> {
        if fd < self.fds.len() {
            if self.fds[fd].take().is_some() {
                return Ok(());
            }
        }
        Err(FdError::BadRequest)
    }
}
