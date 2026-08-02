extern crate alloc;

use crate::ramdisk::TarFileSystem;
use crate::syscall::SYS_UNAME;
use crate::uart::{Uart, UartError};
use crate::version::UtsName;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7F;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

pub fn run(uart: &mut Uart, ramdisk: Option<TarFileSystem>) -> ! {
    let _ = writeln!(uart, "\nmitosOS shell -- type 'help' for commands.");
    let mut current_line = String::new();
    let mut history: Vec<String> = Vec::new();
    let _ = write!(uart, "mitosOS> ");

    loop {
        // Mask interrupts before checking
        #[cfg(target_arch = "x86_64")]
        unsafe { core::arch::asm!("cli", options(nomem, nostack, preserves_flags)) };
        #[cfg(target_arch = "aarch64")]
        unsafe { core::arch::asm!("msr daifset, #2", options(nomem, nostack)) };

        if let Some(byte) = crate::interrupts::dequeue_byte() {
            // Unmask before doing real work
            #[cfg(target_arch = "x86_64")]
            unsafe { core::arch::asm!("sti", options(nomem, nostack, preserves_flags)) };
            #[cfg(target_arch = "aarch64")]
            unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack)) };

            match byte {
                CR | LF => {
                    let _ = writeln!(uart);
                    let trimmed = current_line.trim();
                    if !trimmed.is_empty() {
                        history.push(String::from(trimmed));
                        run_command(uart, trimmed, &history, &ramdisk);
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
            // Atomic unmask + sleep
            #[cfg(target_arch = "x86_64")]
            unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack, preserves_flags)) };
            #[cfg(target_arch = "aarch64")]
            unsafe { core::arch::asm!("msr daifclr, #2", "wfe", options(nomem, nostack)) };
        }
    }
}

fn run_command(uart: &mut Uart, line: &str, history: &[String], ramdisk: &Option<TarFileSystem>) {
    let args: Vec<&str> = line.split_whitespace().collect();
    if args.is_empty() {
        return;
    }
    let cmd = args[0];

    match cmd {
        "help" => {
            let _ = writeln!(
                uart,
                "commands: help, about, uname, translate <hex vaddr>, ps, echo <text>, history, memstat, panic, ls, cat <file>, stat <file>, raw <file>, rxtest, diskread <lba> [count], run <file>"
            );
        }
        "about" => {
            let arch = if cfg!(target_arch = "x86_64") {
                "x86_64 (Intel/AMD Bare-Metal)"
            } else if cfg!(target_arch = "aarch64") {
                "aarch64 (ARM64 Bare-Metal)"
            } else {
                "Unknown Architecture"
            };
            let _ = writeln!(
                uart,
                "mitosOS Phase 1 -- Engine: O(1) Allocator Core | Target: {}",
                arch
            );
        }
        "uname" => {
            cmd_uname(uart);
        }
        "translate" => {
            if args.len() < 2 {
                let _ = writeln!(uart, "Usage: translate <hex vaddr>");
                return;
            }
            let hex = args[1].strip_prefix("0x").unwrap_or(args[1]);
            let vaddr = match usize::from_str_radix(hex, 16) {
                Ok(n) => n,
                Err(_) => {
                    let _ = writeln!(uart, "Invalid address: '{}'", args[1]);
                    return;
                }
            };
            let (root, _is_ring3) = crate::task::current_task_access_info();
            if root == 0 {
                let _ = writeln!(uart, "No active task page table yet.");
                return;
            }
            let result = unsafe {
                crate::vmm::arch::translate_addr(root as *const crate::vmm::arch::PageTable, vaddr)
            };
            match result {
                Some(phys) => {
                    let _ = writeln!(uart, "{:#x} -> {:#x}", vaddr, phys);
                }
                None => {
                    let _ = writeln!(uart, "{:#x} is not mapped in the current task's page table", vaddr);
                }
            }
        }
        "ps" => {
            let tasks = crate::task::get_task_list();
            let _ = writeln!(uart, "--- mitosOS Task Table ---");
            let _ = writeln!(uart, "ID | Parent | State | Memory Root");
            let _ = writeln!(uart, "----+--------+-----------+-------------------");
            for t in tasks {
                let state_str = match t.state {
                    crate::task::TaskState::Ready => "Ready",
                    crate::task::TaskState::Running => "Running",
                    crate::task::TaskState::Blocked => "Blocked", 
                    crate::task::TaskState::Terminated => "Terminated",
                };
                let _ = writeln!(
                    uart,
                    "{:<3} | {:<6} | {:<9} | 0x{:016x}",
                    t.id, t.parent_id, state_str, t.memory_root
                );
            }
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
                let _ = writeln!(uart, " {}: {}", index + 1, logged_cmd);
            }
        }
        "memstat" => {
            let _ = writeln!(uart, "--- Memory System Architecture ---");
            let _ = writeln!(uart, " Heap Location Range : 0x150000 -> 0x1F0000");
            let _ = writeln!(uart, " Heap Arena Budget : 640 KiB Active Managed Space");
            let _ = writeln!(uart, " Alloc Engine Speed : Hardened Hardware Bit-Scanning O(1)");
            match crate::memory::vmm_alloc_frame() {
                Some(addr) => {
                    let _ = writeln!(uart, " Physical Frame Alloc: 0x{:08x} (demo allocation)", addr);
                }
                None => {
                    let _ = writeln!(uart, " Physical Frame Alloc: frame pool exhausted");
                }
            }
        }
        "panic" => {
            let _ = write!(uart, "Trigger test panic? (y/N): ");
            let response = uart.read_byte();
            uart.write_byte(response);
            let _ = writeln!(uart);
            if response == b'y' || response == b'Y' {
                panic!("shell-triggered test panic");
            } else {
                let _ = writeln!(uart, "Aborted.");
            }
        }
        "rxtest" => {
            let _ = writeln!(uart, "Listening for one byte (bounded poll, bypasses IRQ queue)...");
            match uart.try_read_byte() {
                Ok(b) => {
                    let _ = writeln!(uart, "Got byte: 0x{:02X}", b);
                }
                Err(UartError::Timeout) => {
                    let _ = writeln!(uart, "Timed out waiting for input.");
                }
                Err(UartError::LineError) => {
                    let _ = writeln!(uart, "Line error (framing/parity/overrun) detected and discarded.");
                }
            }
        }
        "diskread" => {
            #[cfg(target_arch = "x86_64")]
            {
                if args.len() < 2 {
                    let _ = writeln!(uart, "Usage: diskread <lba> [count]");
                    return;
                }
                let lba: u32 = match args[1].parse() {
                    Ok(n) => n,
                    Err(_) => {
                        let _ = writeln!(uart, "Invalid LBA: '{}'", args[1]);
                        return;
                    }
                };
                let count: u32 = if args.len() >= 3 {
                    match args[2].parse() {
                        Ok(n) => n,
                        Err(_) => {
                            let _ = writeln!(uart, "Invalid sector count: '{}'", args[2]);
                            return;
                        }
                    }
                } else {
                    1
                };
                if count == 0 || count > 64 {
                    let _ = writeln!(uart, "count must be between 1 and 64 sectors (kept small: 640 KiB heap)");
                    return;
                }

                let mut buf = alloc::vec![0u8; count as usize * 512];
                match crate::fs::ata::read_sectors(lba, count, &mut buf) {
                    Ok(()) => {
                        let _ = writeln!(uart, "--- LBA {} .. {} (first 32 bytes) ---", lba, lba + count - 1);
                        for chunk in buf[..32.min(buf.len())].chunks(16) {
                            for b in chunk {
                                let _ = write!(uart, "{:02x} ", b);
                            }
                            let _ = writeln!(uart);
                        }
                        if buf.len() >= 512 {
                            let sig = u16::from_le_bytes([buf[510], buf[511]]);
                            let _ = writeln!(
                                uart,
                                "sector 0 boot signature: 0x{:04x} ({})",
                                sig,
                                if sig == 0xAA55 { "valid" } else { "not a boot sector" }
                            );
                        }
                    }
                    Err(e) => {
                        let _ = writeln!(uart, "ATA read error: {:?}", e);
                    }
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                let _ = writeln!(uart, "diskread is only available on x86_64 (ATA PIO driver).");
            }
        }
        "ls" => {
            if let Some(fs) = ramdisk {
                let _ = writeln!(uart, "--- Ramdisk Contents ---");
                for file in fs.files() {
                    if file.is_file() {
                        let _ = writeln!(uart, " [{:06} bytes] {}", file.size, file.name);
                    } else if file.is_dir() {
                        let _ = writeln!(uart, " [ DIR ] {}/", file.name);
                    }
                }
            } else {
                let _ = writeln!(uart, "Error: No ramdisk mounted.");
            }
        }
        "cat" => {
            if args.len() < 2 {
                let _ = writeln!(uart, "Usage: cat <file>");
                return;
            }
            let target_file = args[1];
            let vfs = crate::fs::vfs::VFS.lock();
            if let Some(node) = vfs.open(target_file) {
                let meta = node.metadata();
                let mut buffer = alloc::vec![0u8; meta.size];
                match node.read(0, &mut buffer) {
                    Ok(bytes_read) => {
                        if let Ok(text) = core::str::from_utf8(&buffer[..bytes_read]) {
                            let _ = write!(uart, "{}", text);
                        } else {
                            let _ = writeln!(uart, "[Binary file, size: {} bytes]", bytes_read);
                        }
                    }
                    Err(e) => {
                        let _ = writeln!(uart, "Error reading file: {}", e);
                    }
                }
            } else {
                let _ = writeln!(uart, "Error: File '{}' not found in VFS.", target_file);
            }
        }
        "stat" => {
            if args.len() < 2 {
                let _ = writeln!(uart, "Usage: stat <file>");
                return;
            }
            let target_file = args[1];
            let vfs = crate::fs::vfs::VFS.lock();
            if let Some(node) = vfs.open(target_file) {
                let meta = node.metadata();
                let kind = match meta.node_type {
                    crate::fs::NodeType::File => "file",
                    crate::fs::NodeType::Directory => "directory",
                };
                let _ = writeln!(uart, "name: {}", meta.name);
                let _ = writeln!(uart, "type: {}", kind);
                let _ = writeln!(uart, "size: {} bytes", meta.size);
            } else {
                let _ = writeln!(uart, "Error: '{}' not found in VFS.", target_file);
            }
        }
        "run" => {
            if args.len() < 2 {
                let _ = writeln!(uart, "Usage: run <file>");
                return;
            }

            let target_file = args[1];
            let vfs = crate::fs::vfs::VFS.lock();
            let node = match vfs.open(target_file) {
                Some(n) => n,
                None => {
                    let _ = writeln!(uart, "Error: '{}' not found in VFS.", target_file);
                    return;
                }
            };
            let meta = node.metadata();
            let mut buffer = alloc::vec![0u8; meta.size];
            let bytes_read = match node.read(0, &mut buffer) {
                Ok(n) => n,
                Err(e) => {
                    let _ = writeln!(uart, "Error reading file: {}", e);
                    return;
                }
            };
            // node is an Arc, not borrowed from vfs -- safe to release
            // the lock before spawning, since load_elf_to_process /
            // spawn_from_elf never touch the VFS themselves and there's
            // no reason to hold it while they run.
            drop(vfs);

            let _ = writeln!(uart, "Loading '{}' ({} bytes)...", target_file, bytes_read);
            if crate::task::spawn_from_elf(&buffer[..bytes_read]) {
                let _ = writeln!(uart, "Spawned as a new isolated (ring-3) process -- check 'ps'.");
            } else {
                let _ = writeln!(
                    uart,
                    "Failed to spawn '{}'. Common causes right now: no free task slot \
                     (MAX_TASKS is 4, and two are already used by the background workers), \
                     or the ELF failed to load/map -- see task.rs::spawn_from_elf.",
                    target_file
                );
            }
        }
        "raw" => {
            if args.len() < 2 {
                let _ = writeln!(uart, "Usage: raw <file>");
                return;
            }
            if let Some(fs) = ramdisk {
                if let Some(entry) = fs.find(args[1]) {
                    if entry.is_file() {
                        match entry.as_text() {
                            Some(text) => {
                                let _ = write!(uart, "{}", text);
                            }
                            None => {
                                let _ = writeln!(uart, "[Binary file, {} bytes]", entry.size);
                            }
                        }
                    } else {
                        let _ = writeln!(uart, "'{}' is not a regular file.", args[1]);
                    }
                } else {
                    let _ = writeln!(uart, "Error: '{}' not found on ramdisk.", args[1]);
                }
            } else {
                let _ = writeln!(uart, "Error: No ramdisk mounted.");
            }
        }
        _ => {
            let _ = writeln!(uart, "unknown command: {} (try 'help')", cmd);
        }
    }
}

/// Executes the `uname` shell command by triggering the SYS_UNAME system call.
fn cmd_uname(uart: &mut Uart) {
    let mut info = UtsName::new();
    let ptr = &mut info as *mut UtsName as usize;
    let res: usize;

    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov rbx, {ptr_reg}",
            "int 0x80",
            "pop rbx",
            ptr_reg = in(reg) ptr,
            in("rax") SYS_UNAME,
            lateout("rax") res,
        );
    }

    #[cfg(target_arch = "aarch64")]
    {
        // A direct `svc #0` here would trap to the "Current EL, SP_x"
        // vector, not the "Lower EL" one used by real EL0 syscalls --
        // AArch64 routes a software exception based on whether the
        // caller was already *at* the target EL (EL1, always, for
        // svc), unlike x86's `int N`, which doesn't distinguish. The
        // shell runs at EL1, so this used to land in
        // handle_el1_sync_exception and panic the kernel every time
        // this command ran ("SVC instruction (unexpected at EL1)").
        // There's no actual privilege boundary to cross here anyway
        // -- the shell already *is* kernel code -- so just call the
        // handler directly instead of manufacturing a trap.
        res = crate::syscall::syscall_handler(SYS_UNAME, ptr, 0, 0);
    }

    if res == 0 {
        let sysname = core::str::from_utf8(&info.sysname)
            .unwrap_or("?")
            .trim_matches('\0');
        let version = core::str::from_utf8(&info.version)
            .unwrap_or("?")
            .trim_matches('\0');
        let machine = core::str::from_utf8(&info.machine)
            .unwrap_or("?")
            .trim_matches('\0');
        let _ = writeln!(uart, "{sysname} v{version} ({machine})");
    } else {
        let _ = writeln!(uart, "Error executing uname syscall.");
    }
}
