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

// --- AArch64 (Raspberry Pi) Specifics ---
#[cfg(target_arch = "aarch64")]
pub fn pi_init() {
    // This is where you will add Pi-specific initialization
    // such as UART setup or interrupt controller configuration.
}

// --- Common Entry Point ---
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // If we are on Pi, run Pi-specific initialization first
    #[cfg(target_arch = "aarch64")]
    pi_init();

    crate::kernel_main();
}
