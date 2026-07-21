//! System Call Dispatcher layer for mitosOS.
//! Handles requests from user/kernel tasks via software interrupts (svc / int).

use core::fmt::Write;
use alloc::sync::Arc;
use crate::fs::vfs::VFS;

// System Call Numbers
pub const SYS_WRITE: usize = 1;
pub const SYS_READ: usize = 2;
pub const SYS_EXIT: usize = 60;

/// Central System Call Dispatcher
/// Called from architecture-specific trap/interrupt handlers, passing raw registers.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_handler(sys_num: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    match sys_num {
        SYS_WRITE => {
            let fd = arg1;
            let ptr = arg2 as *const u8;
            let len = arg3;

            if fd == 1 || fd == 2 {
                if !ptr.is_null() && len > 0 {
                    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
                    let mut uart = unsafe { crate::uart::Uart::init() };
                    let _ = uart.write_str(core::str::from_utf8(slice).unwrap_or("?"));
                    return len;
                }
            }
            usize::MAX
        }
        SYS_READ => {
            let fd = arg1;
            let ptr = arg2 as *mut u8;
            let len = arg3;

            if fd == 0 {
                if !ptr.is_null() && len > 0 {
                    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
                    let mut bytes_read = 0;
                    while bytes_read < len {
                        if let Some(byte) = crate::interrupts::dequeue_byte() {
                            slice[bytes_read] = byte;
                            bytes_read += 1;
                        } else {
                            break;
                        }
                    }
                    return bytes_read;
                }
            }
            usize::MAX
        }
        SYS_EXIT => {
            let exit_code = arg1;
            crate::task::exit(); 
            exit_code
        }
        _ => {
            let mut uart = unsafe { crate::uart::Uart::init() };
            let _ = writeln!(uart, "mitosOS: Unknown syscall number: {sys_num}");
            usize::MAX
        }
    }
}
