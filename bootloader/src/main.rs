#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    unsafe {
        asm!(
            "mov ah, 0x0e",
            "mov al, 'X'",
            "int 0x10",
            out("ax") _,
            options(nomem, nostack, preserves_flags)
        );
    }

    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
