// Only import limine items when targeting x86_64
#[cfg(target_arch = "x86_64")]
use limine::BaseRevision;
#[cfg(target_arch = "x86_64")]
use limine::request::{KernelAddressRequest, StackSizeRequest};

// Only define Limine protocol requests when targeting x86_64
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
static KERNEL_ADDRESS: KernelAddressRequest = KernelAddressRequest::new();

// This entry point remains active for all architectures
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    crate::kernel_main();
}
