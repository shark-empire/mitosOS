#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    // We use inline assembly to call BIOS interrupt 0x10
    // AH=0x0E is the function for "Teletype Output"
    // AL contains the character to print
    unsafe {
        asm!(
            "mov ah, 0x0e",
            "mov al, 'X'",
            "int 0x10",
            options(nomem, nostack, preserves_flags)
        );
    }

    // Infinite loop to prevent the CPU from executing random memory
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
