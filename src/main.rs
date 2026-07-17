#![no_std]
#![no_main]

mod boot;
mod shell;
mod uart;
mod memory;
use memory::BitmapAllocator;

// Manage up to 1GB of RAM (1GB / 4KB = 262,144 frames)
// 262,144 / 64 bits per u64 = 4,096 u64s in our bitmap array
static mut PAGE_ALLOCATOR: BitmapAllocator<4096> = BitmapAllocator::new();


use core::fmt::Write;
use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

    // Phase 0: an interactive shell over UART — see SECURITY.md for what
    // comes next (exception levels, paging, user mode, and eventually a
    // real network stack + SSH, using audited crates for the crypto).
    shell::run(&mut uart);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Re-initialising here is safe: it only rewrites GPIO/UART config
    // registers, and nothing else is running to race with it.
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: PANIC: {info}");
    park();
}

fn park() -> ! {
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
