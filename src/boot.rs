// --- x86_64 (Limine) Specifics ---
#[cfg(target_arch = "x86_64")]
use limine::BaseRevision;
#[cfg(target_arch = "x86_64")]
use limine::request::{ExecutableAddressRequest, StackSizeRequest};

#[cfg(target_arch = "x86_64")]
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[cfg(target_arch = "x86_64")]
#[used]
#[unsafe(link_section = ".requests")]
static STACK: StackSizeRequest = StackSizeRequest::new().with_size(65536);

#[cfg(target_arch = "x86_64")]
#[used]
#[unsafe(link_section = ".requests")]
static KERNEL_ADDRESS: ExecutableAddressRequest = ExecutableAddressRequest::new();

// --- Limine Entry Point (x86_64) ---
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    crate::kernel_main();
}

// --- Pi Entry Point (AArch64) ---
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    // Perform any Pi-specific setup here
    crate::kernel_main();
}
