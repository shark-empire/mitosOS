use limine::request::{KernelAddressRequest, StackSizeRequest};

// Tells the bootloader we want a stack of 64KB
static STACK: LimineStackSizeRequest = LimineStackSizeRequest::new(65536);

// Tells the bootloader we want to be at a specific address
static KERNEL_ADDRESS: LimineKernelAddressRequest = LimineKernelAddressRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static REQUESTS: [&dyn limine::LimineRequest; 2] = [
    &STACK,
    &KERNEL_ADDRESS,
];

// This is the entry point that the bootloader calls
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    crate::kernel_main();
}
