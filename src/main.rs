#![no_std]
#![no_main]

// REMOVED: mod boot; (Delete src/boot.rs entirely)

mod shell;
mod uart;
mod memory;
use memory::BitmapAllocator;

static mut PAGE_ALLOCATOR: BitmapAllocator<4096> = BitmapAllocator::new();

use core::fmt::Write;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn kernel_main() -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

    shell::run(&mut uart);
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
