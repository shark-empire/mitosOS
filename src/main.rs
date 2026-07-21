#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

// Unlocks Rust's official smart pointers and collections (Box, Vec, String, etc.)
extern crate alloc;

mod interrupts;
mod memory;
mod ramdisk; // <--- ADDED: Ramdisk module
mod shell;
mod uart;
mod fs;
pub mod task;

use core::fmt::Write;
use core::panic::PanicInfo;
use memory::BitmapAllocator;

// Standard legacy bare-metal allocator state.
static mut PAGE_ALLOCATOR: BitmapAllocator<4096> = BitmapAllocator::new();

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };

    unsafe {
        // 1. Install IDT/Vector table first so the CPU knows how to handle traps/IRQs.
        interrupts::init();
        
        // 2. Initialize the ultra-fast O(1) heap allocator subsystem.
        // (Note: ensure your .bss doesn't collide with 0x150_000 as the kernel grows).
        memory::init_memory_subsystem(0x150_000, 0xA0_000);
        
        // 3. Unmask the UART's interrupt line at the hardware/controller level.
        uart.enable_interrupts();
        
        // 4. Finally, unmask interrupts at the CPU level (sets RFLAGS.IF / clears PSTATE.I).
        interrupts::enable_cpu_interrupts();
    }

    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

    // --- ADDED: Ramdisk Initialization ---
    // Use the embedded tarball for ARM64, and the memory address for x86_64
    let inited: Option<ramdisk::TarFileSystem> = {
        #[cfg(target_arch = "aarch64")]
        {
            ramdisk::TarFileSystem::new_embedded()
        }
        
        #[cfg(target_arch = "x86_64")]
        {
            unsafe { ramdisk::TarFileSystem::new(0x200_000, 0x20_000) }
        }
    };

    if inited.is_some() {
        let _ = writeln!(uart, "mitosOS: initrd detected and mounted successfully.");
    } else {
        let _ = writeln!(uart, "mitosOS: no valid initrd found at target address.");
    }

    unsafe {
        crate::task::spawn(background_worker);
    }
    // -------------------------------------

    // Pass the initrd to the shell
    shell::run(&mut uart, inited);
}

// Background worker module:
extern "C" fn background_worker() -> ! {
    loop {
        // You can write a tiny UART print or loop here to prove it's running
        // e.g., printing a dot or ticker every time it cycles
        for _ in 0..10000000 {
            core::hint::spin_loop();
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: PANIC: {info}");
    park();
}

fn park() -> ! {
    loop {
        // Use atomic unmask-and-sleep to prevent race conditions during panic halts
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack, preserves_flags));
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr daifset, #2", "wfe", options(nomem, nostack));
        }
    }
}
