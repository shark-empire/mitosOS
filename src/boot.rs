#![no_std]

use limine::request::{KernelAddressRequest, StackSizeRequest};
use limine::LiminePtr;

// Tells the bootloader we want a stack of 64KB
static STACK: StackSizeRequest = StackSizeRequest::new(65536);

// Tells the bootloader we want to be at a specific address
static KERNEL_ADDRESS: KernelAddressRequest = KernelAddressRequest::new();

#[used]
#[link_section = ".requests"]
static REQUESTS: [LiminePtr; 2] = [
    LiminePtr::new(&STACK),
    LiminePtr::new(&KERNEL_ADDRESS),
];

// This is the entry point that the bootloader calls
#[no_mangle]
pub extern "C" fn _start() -> ! {
    crate::kernel_main();
}
