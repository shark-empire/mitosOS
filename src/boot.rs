use limine::BaseRevision;
use limine::request::{KernelAddressRequest, StackSizeRequest};

// Declares the protocol revision we're using — add this even though it
// wasn't in your original file; see note below.
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

// Tells the bootloader we want a stack of 64KB
#[used]
#[unsafe(link_section = ".requests")]
static STACK: StackSizeRequest = StackSizeRequest::new().with_size(65536);

// Tells the bootloader we want to know where the kernel was loaded
#[used]
#[unsafe(link_section = ".requests")]
static KERNEL_ADDRESS: KernelAddressRequest = KernelAddressRequest::new();

// This is the entry point that the bootloader calls
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    crate::kernel_main();
}
