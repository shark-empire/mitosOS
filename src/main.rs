#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

// Unlocks Rust's official smart pointers and collections (Box, Vec, String, etc.)
extern crate alloc;

mod fs;
mod interrupts;
mod memory;
mod ramdisk;
mod shell;
pub mod task;
mod uart;
mod vmm;
pub mod sync;
pub mod syscall;
pub mod version;
pub mod timer;
mod elf;
mod fd;


use core::fmt::Write;
use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };

    unsafe {
        // 1. Install IDT/Vector table so the CPU can handle exceptions & IRQs.
        interrupts::init();

        // 2. Initialize the heap allocator subsystem.
        memory::init_memory_subsystem(0x150_000, 0xA0_000);
        memory::protect_boot_memory(0x1F0_000);
        vmm_self_test(&mut uart);
        
        // 3. Initialize the hardware timer frequencies
        crate::timer::hardware::init();

        // 4. Unmask the UART's interrupt line.
        uart.enable_interrupts();
        
        // STOP! Do not call `interrupts::enable_cpu_interrupts()` yet!
    }

    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

    // --- Ramdisk & VFS Mounting ---
    let inited: Option<ramdisk::TarFileSystem> = {
        #[cfg(target_arch = "aarch64")]
        {
            ramdisk::TarFileSystem::new_embedded()
        }
        #[cfg(target_arch = "x86_64")]
        {
            unsafe { ramdisk::TarFileSystem::new(0x200_000, 0x20_000) }
        }
    };

    if let Some(tar_fs) = inited {
        let adapter = alloc::sync::Arc::new(crate::fs::tar_adapter::TarFsAdapter::new(tar_fs));
        crate::fs::vfs::VFS.lock().mount("/", adapter);
        let _ = writeln!(uart, "mitosOS: initrd detected and VFS mounted at /");
    } else {
        let _ = writeln!(uart, "mitosOS: WARN - No valid initrd found.");
    }
    
    // Example of how to use your new ELF loader:
if let Some(file_data) = crate::fs::vfs::VFS.lock().read_file("/bin/test_program") {
    if let Ok(process_root) = unsafe { crate::memory::create_process_page_table() } {
        match crate::elf::load_elf_to_process(&file_data, process_root) {
            Ok(entry_point) => {
                let mut uart = unsafe { crate::uart::Uart::init() };
                let _ = writeln!(uart, "ELF loaded successfully! Entry point: {:#x}", entry_point);
            }
            Err(e) => {
                let mut uart = unsafe { crate::uart::Uart::init() };
                let _ = writeln!(uart, "Failed to load ELF: {}", e);
            }
        }
    }
}


    // --- Spawn Background Worker Tasks ---
    // Spawn these FIRST so the scheduler has something to swap to.
    crate::task::spawn(background_worker, crate::task::ExecutionMode::SharedThread);
    crate::task::spawn(background_worker_2, crate::task::ExecutionMode::SharedThread);

    // --- Enable Preemptive Multitasking ---
    unsafe {
        // NOW unmask CPU-level interrupts (STI / DAIFCLR).
        // The hardware timer will begin ticking immediately after this line.
        interrupts::enable_cpu_interrupts();
    }

    // --- Start Kernel Shell ---
    shell::run(&mut uart, inited);
}


/// Builds a scratch page table and maps two pages through it, purely to
/// exercise vmm.rs's table-walk and `MapFlags` encoding. Never installed
/// as the active table, so it's inert with respect to the kernel's real
/// memory model.
unsafe fn vmm_self_test(uart: &mut uart::Uart) {
    let Some(root_frame) = memory::vmm_alloc_frame() else {
        let _ = writeln!(uart, "mitosOS: VMM self-test skipped (no free frame for root table)");
        return;
    };

    unsafe {
        core::ptr::write_bytes(root_frame as *mut u8, 0, 4096);
    }
    let root = root_frame as *mut vmm::arch::PageTable;

    let data_result =
        unsafe { vmm::arch::map_page(root, 0x150_000, 0x150_000, memory::MapFlags::kernel_data()) };
    let code_result =
        unsafe { vmm::arch::map_page(root, 0x400_000, 0x400_000, memory::MapFlags::kernel_code()) };

    match (data_result, code_result) {
        (Ok(()), Ok(())) => {
            let _ = writeln!(uart, "mitosOS: VMM self-test OK (2 pages mapped in scratch table)");
        }
        (d, c) => {
            let _ = writeln!(uart, "mitosOS: VMM self-test FAILED: data={:?} code={:?}", d, c);
        }
    }
}

/// Background worker task demonstrating preemptive task execution
extern "C" fn background_worker() -> ! {
     loop {
        let mut uart = unsafe { crate::uart::Uart::init() };
        let _ = core::fmt::Write::write_str(&mut uart, "[Worker 1: Tick]\n");
        
        // Spin to waste time. The hardware timer should interrupt this automatically!
        for _ in 0..5_000_000 {
            core::hint::spin_loop();
        }
        
        // NOTICE: No yield_now() here anymore!
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: PANIC: {info}");
    park();
}

/// Called by the allocator when a heap allocation can't be satisfied.
/// Without this, an OOM just aborts silently -- this at least tells you
/// what was being allocated before the kernel halts.
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(
        uart,
        "mitosOS: PANIC: allocation failure (size={}, align={})",
        layout.size(),
        layout.align()
    );
    park();
}

fn park() -> ! {
    loop {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack, preserves_flags));
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr daifset, #2", "wfe", options(nomem, nostack));
        }
    }
}

// Add this function anywhere in src/main.rs
extern "C" fn background_worker_2() -> ! {
    loop {
        let mut uart = unsafe { crate::uart::Uart::init() };
        let _ = core::fmt::Write::write_str(&mut uart, "[Worker 2: Tick]\n");
        for _ in 0..5_000_000 {
            core::hint::spin_loop();
                          }
    }
}
