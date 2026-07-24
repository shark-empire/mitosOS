// Repo path: src/main.rs
#![no_std]
#![no_main]

// Unlocks Rust's official smart pointers and collections (Box, Vec, String, etc.)
extern crate alloc;

mod block;
mod fs;
mod interrupts;
mod memory;
mod ramdisk;
mod shell;
mod elf;
mod fd;
mod graphics;
mod timer;
mod vmm;
pub mod task;
mod uart;
pub mod sync;
pub mod syscall;
pub mod version;
pub mod pci;


use core::fmt::Write;
use alloc::vec;
use core::panic::PanicInfo;
use crate::memory::{protect_boot_memory, MapFlags};
use crate::graphics::{Framebuffer, Color};
use crate::fd::FileDescriptorTable;
use crate::ramdisk::TarFileSystem;
use alloc::boxed::Box;





#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };

    unsafe {
        // 1. Install IDT/Vector table so the CPU can handle exceptions & IRQs.
        interrupts::init();

        // 2. Initialize the heap allocator subsystem.
        // (Ensures .bss doesn't collide with 0x150_000 as kernel grows).
        memory::init_memory_subsystem(0x150_000, 0xA0_000);

        // 3. Unmask the UART's interrupt line.
        uart.enable_interrupts();

        // 4. Unmask CPU-level interrupts (STI on x86, DAIFCLR on ARM64).
        interrupts::enable_cpu_interrupts();
    }
    
    
    // Scan the PCI bus
let pci_devices = crate::pci::scan_buses();
crate::println!("--- PCI Devices Found ---");

for dev in pci_devices {
    crate::println!(
        "Bus {} Slot {}: Vendor 0x{:X} Device 0x{:X} | Class 0x{:02X} Subclass 0x{:02X}",
        dev.bus, dev.slot, dev.vendor_id, dev.device_id, dev.class, dev.subclass
    );
    
    // Check specifically for an AHCI Controller
    // Class 0x01 = Mass Storage, Subclass 0x06 = SATA
    if dev.class == 0x01 && dev.subclass == 0x06 {
        crate::println!(">>> FOUND AHCI CONTROLLER! BAR5 Address: 0x{:X} <<<", dev.bar5);
    }
}
crate::println!("-------------------------");

    

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

    // 1. MEMORY: Protect bootloader memory and set flags
    unsafe {
        protect_boot_memory(0x100000); // 0x100000 is a placeholder kernel end address
        let _code = MapFlags::kernel_code();
        let _data = MapFlags::kernel_data();
    }

    // 2. GRAPHICS: Initialize the screen
    unsafe {
        // Placeholders for framebuffer address, width, height, and pitch
        let mut fb = Framebuffer::new(0xFD000000, 1024, 768, 4096);
        fb.clear(Color::BLACK);
        fb.draw_string(10, 10, "mitosOS System Init...", Color::GREEN);
    }

    // 3. HARDWARE: Start the timer
   timer::hardware::init();


    // 4. FILESYSTEM: Load the Ramdisk
    if let Some(_ramdisk) = TarFileSystem::new_embedded() {
        // Ramdisk successfully loaded into memory
    }

    // 5. USERSPACE: Prepare file descriptor table
    let mut _root_fd_table = FileDescriptorTable::new();

    // --- FAT32 Mounting (RAM-backed test volume) ---
    // RamBlockDevice starts zeroed, so mount() will fail with "Invalid boot
    // sector signature" until real FAT32 bytes are written into it -- either
    // seed it from an embedded test image, or swap RamBlockDevice for a real
    // disk once fs::ata's BlockDevice impl is ready. That failure path is
    // expected right now, not a bug -- it's handled below, not panicking.
    //
    // 256 sectors = 128KB, sized to comfortably fit your ~640KB heap. A real
    // FAT32 volume needs far more than that in practice, so treat this as
    // "prove the wiring works," not a production-sized mount.
    let ram_disk: alloc::boxed::Box<dyn block::BlockDevice> =
        alloc::boxed::Box::new(block::RamBlockDevice::new(256));

    match crate::fs::fat32::Fat32FileSystem::mount(ram_disk) {
        Ok(fat_fs) => {
            let fat_adapter = alloc::sync::Arc::new(crate::fs::fat32_adapter::Fat32Adapter::new(fat_fs));
            crate::fs::vfs::VFS.lock().mount("/disk", fat_adapter);
            let _ = writeln!(uart, "mitosOS: FAT32 volume mounted at /disk");
        }
        Err(e) => {
            let _ = writeln!(uart, "mitosOS: FAT32 mount skipped ({e})");
        }
    }
    
    // In src/main.rs or a storage initialization function:
// To this (using an available ATA device or block device initializer):
#[cfg(target_arch = "x86_64")]
let ata_device = crate::fs::ata::AtaDevice::new(); // Or your specific initialization method
// src/main.rs around line 134–136


#[cfg(target_arch = "aarch64")]
let block_device: Box<dyn crate::block::BlockDevice> = Box::new(crate::block::RamBlockDevice::new(2048));

#[cfg(target_arch = "x86_64")]
let block_device: Box<dyn crate::block::BlockDevice> = Box::new(crate::fs::ata::AtaDevice::new().expect("Failed to init ATA"));

let mut fat32_fs = crate::fs::fat32::Fat32FileSystem::mount(block_device)
    .expect("FAT32 mount failed");


let content = fat32_fs.read_file_by_path("/test.txt");


    // --- Spawn Background Worker Task ---
    crate::task::spawn(background_worker, crate::task::ExecutionMode::SharedThread);
    crate::task::spawn(background_worker_2, crate::task::ExecutionMode::SharedThread);

    // --- Start Kernel Shell ---
    shell::run(&mut uart, inited);
}

/// Background worker task demonstrating preemptive task execution
extern "C" fn background_worker() -> ! {
    loop {
        // Yield voluntarily or let the hardware timer interrupt switch tasks
        crate::task::yield_now();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    let _ = writeln!(uart, "mitosOS: PANIC: {info}");
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
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
        crate::task::yield_now();
    }
}
