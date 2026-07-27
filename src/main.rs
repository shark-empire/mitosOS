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
mod drivers;
pub mod task;
mod uart;
pub mod sync;
pub mod syscall;
pub mod version;
pub mod addr;
#[cfg(target_arch = "x86_64")]
pub mod pci;
#[cfg(target_arch = "x86_64")]
pub mod gdt;


use core::fmt::Write;
use core::panic::PanicInfo;
use crate::memory::{protect_boot_memory, MapFlags};
use crate::graphics::{Framebuffer, Color};
use crate::fd::FileDescriptorTable;
use crate::ramdisk::TarFileSystem;
use alloc::boxed::Box;

const HEAP_START: usize = 0x150_000;
const HEAP_SIZE: usize = 0xA0_000;

// Provided by linker_x86.ld / linker_rpi.ld: marks the real end of the
// kernel's own image (code+rodata+data+bss). Used by protect_boot_memory
// so the frame allocator never hands out a frame inside the kernel
// itself -- see the comment there for what used to go wrong without it.
unsafe extern "C" {
    static _kernel_end: u8;
}





#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };

    unsafe {
        // 1. Load the kernel's own GDT/TSS (ring-3 segments + the stack
        //    the CPU uses on any trap taken from ring 3). Must run before
        //    interrupts::init() -- the double-fault gate is set to use
        //    IST1, which this sets up.
        #[cfg(target_arch = "x86_64")]
        gdt::init();

        // 2. Install IDT/Vector table so the CPU can handle exceptions & IRQs.
        interrupts::init();

        // 3. Initialize the heap allocator subsystem.
        // (Ensures .bss doesn't collide with 0x150_000 as kernel grows).
        memory::init_memory_subsystem(HEAP_START, HEAP_SIZE);

        // 3b. Reserve boot/kernel/heap memory in the frame allocator.
        // This has to happen right here, before *anything* else gets a
        // chance to call vmm_alloc_frame() -- it used to run much later
        // (after PCI scan, a demo allocation, and AHCI init), so all of
        // those were freely handing out frames from the "reserved" range,
        // including frame 0 itself.
        protect_boot_memory(&raw const _kernel_end as usize, HEAP_START, HEAP_SIZE);

        // 3. Unmask the UART's interrupt line.
        uart.enable_interrupts();

        // 4. Unmask CPU-level interrupts (STI on x86, DAIFCLR on ARM64).
        interrupts::enable_cpu_interrupts();
    }
    let _ = writeln!(uart, "mitosOS: kernel_main reached. Boot OK.");

#[cfg(target_arch = "x86_64")]
{
 let scan_pci_devices = crate::pci::scan_buses();
let _ = writeln!(uart, "--- PCI Devices Found ---");

for dev in scan_pci_devices {
    let _ = writeln!(uart, 
        "Bus {} Slot {}: Vendor 0x{:X} Device 0x{:X} | Class 0x{:02X} Subclass 0x{:02X}",
        dev.bus, dev.slot, dev.vendor_id, dev.device_id, dev.class, dev.subclass
    );
    
    // Check specifically for an AHCI Controller
    // Class 0x01 = Mass Storage, Subclass 0x06 = SATA
    if dev.class == 0x01 && dev.subclass == 0x06 {
        let _ = writeln!(uart, ">>> FOUND AHCI CONTROLLER! BAR5 Address: 0x{:X} <<<", dev.bar5);
    }
}
let _ = writeln!(uart, "-------------------------");
    }


// Test frame allocation during initialization
if let Some(frame) = crate::memory::alloc_frame() {
    let _ = writeln!(uart, "Memory Manager: Allocated physical frame at 0x{:X}", frame);
}

    
    




// Inside src/main.rs
#[cfg(target_arch = "x86_64")]
{
    crate::pci::init_ahci_devices(&mut uart);
}



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

    // 1. MEMORY: flags demo (actual protect_boot_memory now runs right
    // after heap init, near the top of kmain -- see above)
    unsafe {
        let _code = MapFlags::kernel_code();
        let _data = MapFlags::kernel_data();
    }

    // 2. GRAPHICS: Initialize the screen
    const FB_ADDR: usize = 0xFD000000;
    const FB_WIDTH: usize = 1024;
    const FB_HEIGHT: usize = 768;
    const FB_PITCH: usize = 4096;

    let mut fb = unsafe {
        // Identity-map the framebuffer's MMIO pages before anything touches
        // them -- writing through an unmapped physical address page-faults.
        #[cfg(target_arch = "x86_64")]
        {
            let cr3: usize;
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
            let page_table_root = cr3 & !0xFFF;

            let fb_pages = (FB_PITCH * FB_HEIGHT + 0xFFF) / 0x1000;
            for i in 0..fb_pages {
                let addr = FB_ADDR + i * 0x1000;
                if let Err(e) = crate::memory::map_page(page_table_root, addr, addr) {
                    let _ = writeln!(uart, "mitosOS: WARN framebuffer mapping failed: {e}");
                    break;
                }
            }
        }

        Framebuffer::new(FB_ADDR, FB_WIDTH, FB_HEIGHT, FB_PITCH)
    };

    fb.clear(Color::BLACK);
    Framebuffer::draw_boot_splash(&mut fb);
    fb.draw_string(10, 70, "mitosOS System Init...", Color::GREEN);

    // 3. HARDWARE: Start the timer
    timer::hardware::init();
    fb.draw_string(10, 80, "Timer: OK", Color::YELLOW);

    // 4. FILESYSTEM: Load the Ramdisk
    if let Some(_ramdisk) = TarFileSystem::new_embedded() {
        fb.draw_string(10, 90, "Ramdisk: loaded", Color::CYAN);
    } else {
        fb.draw_string(10, 90, "Ramdisk: missing", Color::RED);
    }

    // 5. USERSPACE: Prepare file descriptor table
    let mut _root_fd_table = FileDescriptorTable::new();

    // --- FAT32 Mounting (RAM-backed test volume) ---
    let ram_disk: alloc::boxed::Box<dyn block::BlockDevice> =
        alloc::boxed::Box::new(block::RamBlockDevice::new(256));

    match crate::fs::fat32::Fat32FileSystem::mount(ram_disk) {
        Ok(fat_fs) => {
            let (bps, fats, reserved, spf) = fat_fs.volume_info();
            let _ = writeln!(
                uart,
                "mitosOS: FAT32 volume mounted at /disk ({bps}B/sector, {fats} FAT(s), {reserved} reserved, {spf} sectors/FAT)"
            );
            fb.draw_string(10, 100, "FAT32 (ram): mounted", Color::MAGENTA);
            let fat_adapter = alloc::sync::Arc::new(crate::fs::fat32_adapter::Fat32Adapter::new(fat_fs));
            crate::fs::vfs::VFS.lock().mount("/disk", fat_adapter);
        }
        Err(e) => {
            let _ = writeln!(uart, "mitosOS: FAT32 mount skipped ({e})");
            fb.draw_string(10, 100, "FAT32 (ram): skipped", Color::MAGENTA);
        }
    }

    // --- FAT32 Mounting (real ATA disk) ---
    // Self-tests the drive before handing it to the FAT32 layer, then mounts
    // gracefully -- disk.img in CI is the raw bootable image, not a FAT32
    // volume, so failure here is the expected path, not a panic.
    #[cfg(target_arch = "aarch64")]
    let block_device: Box<dyn crate::block::BlockDevice> = Box::new(crate::block::RamBlockDevice::new(2048));

    #[cfg(target_arch = "x86_64")]
    let block_device: Box<dyn crate::block::BlockDevice> = {
        let mut ata_device = crate::fs::ata::AtaDevice::new().expect("Failed to init ATA");
        match ata_device.self_test() {
            Ok(()) => {
                let _ = writeln!(uart, "mitosOS: ATA self-test passed ({} sectors)", ata_device.total_sectors);
            }
            Err(e) => {
                let _ = writeln!(uart, "mitosOS: ATA self-test FAILED: {e}");
            }
        }
        Box::new(ata_device)
    };

    match crate::fs::fat32::Fat32FileSystem::mount(block_device) {
        Ok(mut fat32_fs) => match fat32_fs.read_file_by_path("/test.txt") {
            Ok(content) => {
                let _ = writeln!(uart, "mitosOS: /test.txt on ATA disk: {} bytes", content.len());
            }
            Err(e) => {
                let _ = writeln!(uart, "mitosOS: ATA /test.txt read skipped ({e})");
            }
        },
        Err(e) => {
            let _ = writeln!(uart, "mitosOS: ATA FAT32 mount skipped ({e})");
        }
    }



    // --- Spawn Background Worker Task ---
    crate::task::spawn(background_worker, crate::task::ExecutionMode::SharedThread, 0);
    crate::task::spawn(background_worker_2, crate::task::ExecutionMode::SharedThread, 0);

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
