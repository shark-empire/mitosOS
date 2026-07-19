Like this?

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use crate::uart::Uart;
use core::fmt::Write;

const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7F;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

pub fn run(uart: &mut Uart) -> ! {
    let _ = writeln!(uart, "\nmitosOS shell -- type 'help' for commands.");
    
    let mut current_line = String::new();
    let mut history: Vec<String> = Vec::new();
    
    let _ = write!(uart, "mitosOS> ");

    loop {
        // Try to pull a byte from the asynchronous interrupt queue
        if let Some(byte) = crate::interrupts::dequeue_byte() {
            match byte {
                CR | LF => {
                    let _ = writeln!(uart);
                    let trimmed = current_line.trim();
                    
                    if !trimmed.is_empty() {
                        history.push(String::from(trimmed));
                        run_command(uart, trimmed, &history);
                    }
                    
                    current_line.clear();
                    let _ = write!(uart, "mitosOS> ");
                }
                BACKSPACE | DELETE => {
                    if !current_line.is_empty() {
                        current_line.pop();
                        uart.write_byte(BACKSPACE);
                        uart.write_byte(b' ');
                        uart.write_byte(BACKSPACE);
                    }
                }
                printable if (0x20..0x7F).contains(&printable) => {
                    if current_line.len() < 1024 {
                        current_line.push(printable as char);
                        uart.write_byte(printable);
                    }
                }
                _ => {}
            }
        } else {
            // THE MAGIC: If there are no keys pressed, put the CPU to sleep!
            // The processor will freeze here using 0% CPU power until the 
            // UART hardware fires an electrical interrupt line to wake it up.
            #[cfg(target_arch = "x86_64")]
            unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };

            #[cfg(target_arch = "aarch64")]
            unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
        }
    }
}

fn run_command(uart: &mut Uart, line: &str, history: &Vec<String>) {
    let args: Vec<&str> = line.split_whitespace().collect();
    if args.is_empty() {
        return;
    }

    let cmd = args[0];

    match cmd {
        "help" => {
            let _ = writeln!(
                uart, 
                "commands: help, about, echo <text>, history, memstat, panic"
            );
        }
        "about" => {
            // Compile-time evaluation changes this string depending on the target matrix build
            #[cfg(target_arch = "x86_64")]
            let arch = "x86_64 (Intel/AMD Bare-Metal)";

            #[cfg(target_arch = "aarch64")]
            let arch = "aarch64 (ARM64 Bare-Metal)";

            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            let arch = "Unknown Architecture";

            let _ = writeln!(
                uart,
                "mitosOS Phase 1 -- Engine: O(1) Allocator Core | Target: {}",
                arch
            );
        }
        "echo" => {
            let payload = &args[1..];
            for (i, word) in payload.iter().enumerate() {
                if i > 0 {
                    let _ = write!(uart, " ");
                }
                let _ = write!(uart, "{}", word);
            }
            let _ = writeln!(uart);
        }
        "history" => {
            let _ = writeln!(uart, "--- Command History Log ---");
            for (index, logged_cmd) in history.iter().enumerate() {
                let _ = writeln!(uart, "  {}: {}", index + 1, logged_cmd);
            }
        }
        "memstat" => {
            let _ = writeln!(uart, "--- Memory System Architecture ---");
            let _ = writeln!(uart, " Heap Location Range : 0x150000 -> 0x1F0000");
            let _ = writeln!(uart, " Heap Arena Budget   : 640 KiB Active Managed Space");
            let _ = writeln!(uart, " Alloc Engine Speed  : Hardened Hardware Bit-Scanning O(1)");
        }
        "panic" => {
            panic!("shell-triggered test panic");
        }
        _ => {
            let _ = writeln!(uart, "unknown command: {} (try 'help')", cmd);
        }
    }
}
