//! A tiny line-based command shell over the UART. This is what mitosOS
//! offers as a "CLI" today — reachable over a physical (or QEMU-emulated)
//! serial connection, not a network. See README for why SSH is a much
//! later phase.

use crate::uart::Uart;
use core::fmt::Write;

const BUF_LEN: usize = 128;
const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7F;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

pub fn run(uart: &mut Uart) -> ! {
    let _ = writeln!(uart, "\nmitosOS shell -- type 'help' for commands.");
    let mut buf = [0u8; BUF_LEN];
    let mut len = 0usize;
    let _ = write!(uart, "mitosOS> ");

    loop {
        let byte = uart.read_byte();

        match byte {
            CR | LF => {
                let _ = writeln!(uart);
                let line = core::str::from_utf8(&buf[..len]).unwrap_or("");
                run_command(uart, line);
                len = 0;
                let _ = write!(uart, "mitosOS> ");
            }
            BACKSPACE | DELETE => {
                if len > 0 {
                    len -= 1;
                    uart.write_byte(BACKSPACE);
                    uart.write_byte(b' ');
                    uart.write_byte(BACKSPACE);
                }
            }
            printable if (0x20..0x7F).contains(&printable) && len < BUF_LEN => {
                buf[len] = printable;
                len += 1;
                uart.write_byte(printable); // local echo
            }
            _ => {
                // Ignore control characters, unprintable bytes, and input
                // past BUF_LEN.
            }
        }
    }
}

fn run_command(uart: &mut Uart, line: &str) {
    let line = line.trim();
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");

    match cmd {
        "" => {}
        "help" => {
            let _ = writeln!(uart, "commands: help, about, echo <text>, panic");
        }
        "about" => {
            let _ = writeln!(
                uart,
                "mitosOS phase 0 -- AArch64, Pi 3B / QEMU raspi3b, no_std, zero deps"
            );
        }
        "echo" => {
            let _ = writeln!(uart, "{rest}");
        }
        "panic" => {
            panic!("shell-triggered test panic");
        }
        _ => {
            let _ = writeln!(uart, "unknown command: {cmd} (try 'help')");
        }
    }
}
