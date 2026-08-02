#![no_std]
#![no_main]

// Pull in the standard macros and functions from our custom OS library
use libmitos::{println, exit};

/// The main entry point mapped by `linker.ld` and executed by `task::spawn_from_elf`.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. Test standard output
    println!("----------------------------------------");
    println!(" mitosOS Ring 3 Initialized Successfully");
    println!("----------------------------------------");
    
    // 2. Test computational logic and formatting
    let sys_name = "mitosOS";
    let arch = "x86_64";
    println!("Running on: {} ({})", sys_name, arch);

    // 3. Graceful termination via SYS_EXIT
    println!("User application shutting down...");
    exit(0);
}
