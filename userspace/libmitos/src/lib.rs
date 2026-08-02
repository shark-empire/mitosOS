#![no_std]

use core::arch::asm;
use core::fmt;
use core::panic::PanicInfo;

// =========================================================================
// System Call Definitions
// =========================================================================

pub mod syscall_numbers {
    pub const SYS_READ: usize = 0;
    pub const SYS_WRITE: usize = 1;
    pub const SYS_MUNMAP: usize = 11;
    pub const SYS_EXIT: usize = 60;
    pub const SYS_UNAME: usize = 63;
}

/// Executes a system call with 1 argument.
#[inline(always)]
unsafe fn syscall1(num: usize, arg1: usize) -> usize {
    let ret: usize;
    asm!(
        "syscall",
        in("rax") num,
        in("rdi") arg1,
        out("rcx") _, // CPU saves RIP here
        out("r11") _, // CPU saves RFLAGS here
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

/// Executes a system call with 3 arguments.
#[inline(always)]
unsafe fn syscall3(num: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let ret: usize;
    asm!(
        "syscall",
        in("rax") num,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        out("rcx") _, 
        out("r11") _, 
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

// =========================================================================
// Safe Rust API Wrappers
// =========================================================================

/// Immediately terminates the current process.
pub fn exit(status: usize) -> ! {
    unsafe { syscall1(syscall_numbers::SYS_EXIT, status); }
    loop { core::hint::spin_loop(); }
}

/// Writes a raw byte buffer to the specified file descriptor.
pub fn write(fd: usize, buf: &[u8]) -> usize {
    unsafe {
        syscall3(
            syscall_numbers::SYS_WRITE,
            fd,
            buf.as_ptr() as usize,
            buf.len()
        )
    }
}

// =========================================================================
// Standard I/O Macros (print! / println!)
// =========================================================================

struct StdOut;

impl fmt::Write for StdOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write(1, s.as_bytes());
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let mut stdout = StdOut;
    let _ = fmt::Write::write_fmt(&mut stdout, args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

// =========================================================================
// Global Panic Handler
// =========================================================================

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n[USER SPACE PANIC] {}", info);
    exit(1);
}
