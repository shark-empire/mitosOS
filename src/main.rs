
#![no_std]
#![no_main]
#![feature(alloc_error_handler)] // Required for custom bare-metal allocation routing

// Unlocks Rust's official smart pointers and collections (Box, Vec, String, etc.)
extern crate alloc;

mod shell;
mod uart;
mod interrupts;
mod memory;
use memory::BitmapAllocator;

static mut PAGE_ALLOCATOR: BitmapAllocator<4096> = BitmapAllocator::new();

use core::fmt::Write;
use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    
   unsafe { // TURN ON THE HARDWARE INTERRUPTS
    uart.enable_interrupts(); 

    // Initialize the ultra-fast O(1) heap allocator subsystem.
    
        memory::init_memory_subsystem(0x150000, 0xA0000);
        interrupts::init();
    }

    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

    shell::run(&mut uart);
}


/// Handles the event where the kernel runs out of dynamic heap memory.
#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(
        uart, 
        "mitosOS: OOM PANIC: Failed to allocate {} bytes with alignment {}", 
        layout.size(), 
        layout.align()
    );
    park();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: PANIC: {info}");
    park();
}

fn park() -> ! {
    loop {
        #[cfg(target_arch = "aarch64")]
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };

        #[cfg(target_arch = "x86_64")]
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
