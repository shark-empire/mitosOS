// --- x86_64 Entry Point ---
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Note: Your custom bootloader must set up a stack 
    // before jumping here, or you must do it in assembly.
    crate::kernel_main();
}

// --- Pi Entry Point (AArch64) ---
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    crate::kernel_main();
}
