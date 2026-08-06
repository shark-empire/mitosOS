// Repo path: src/main.rs
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

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
pub mod process;
mod uart;
pub mod sync;
pub mod syscall;
pub mod version;
pub mod addr;
#[cfg(target_arch = "x86_64")]
pub mod pci;
#[cfg(target_arch = "x86_64")] 
pub mod hal;
#[cfg(target_arch = "x86_64")]
pub mod gdt;
#[cfg(target_arch = "aarch64")]
pub mod mmu;

use core::fmt::Write;
use core::panic::PanicInfo;
use crate::memory::{protect_boot_memory, MapFlags};
#[cfg(target_arch = "x86_64")]
use crate::graphics::{Framebuffer, Color};
use alloc::boxed::Box;

const HEAP_START: usize = 0x150_000;

// x86_64's own bootloader-built identity map (stage2.s) is ~4MiB, so
// this has to stay comfortably inside that regardless of what AArch64
// needs -- proven working as-is, not touched.
#[cfg(target_arch = "x86_64")]
const HEAP_SIZE: usize = 0xA0_000; // 640KB

// AArch64's fallback "disk" (RamBlockDevice::new(2048), used below in
// place of a real ATA/SD driver -- see the FAT32 mounting block)
// allocates exactly 2048 * 512 = 1MiB.
#[cfg(target_arch = "aarch64")]
const HEAP_SIZE: usize = 0x800_000; // 8MB

// Provided by linker_x86.ld / linker_rpi.ld: marks the real end of the
// kernel's own image (code+rodata+data+bss). Used by protect_boot_memory
// so the frame allocator never hands out a frame inside the kernel
// itself -- see the comment there for what used to go wrong without it.
unsafe extern "C" {
    static _kernel_end: u8;
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn kmain() -> ! {
    let mut uart = unsafe { uart::Uart::init() };
    // Checkpoint 0: kmain was reached at all, and the UART's own
    // register pokes (GPIO ALT function, baud divisor, FIFO enable)
    // didn't leave it unable to transmit.
    let _ = writeln!(uart, "[boot] 0: kmain reached, uart live");

    unsafe {
        // 1. Load the kernel's own GDT/TSS (ring-3 segments + the stack
        //    the CPU uses on any trap taken from ring 3). Must run before
        //    interrupts::init() -- the double-fault gate is set to use
        //    IST1, which this sets up.
        #[cfg(target_arch = "x86_64")]
        gdt::init();
        #[cfg(target_arch = "x86_64")]
        hal::init();

        // 2. Install IDT/Vector table so the CPU can handle exceptions & IRQs.
        interrupts::init();
        let _ = writeln!(uart, "[boot] 1: interrupts::init() returned");

        // 2b. Tear down the bootloader's temporary lower-half identity
        //     mapping now that a real IDT is live -- see
        //     memory::unmap_low_half_identity_map's doc comment for why
        //     this moved out of stage2.s and has to run after
        //     interrupts::init(), not before it.
        #[cfg(target_arch = "x86_64")]
        memory::unmap_low_half_identity_map();

        // 3. Initialize the heap allocator subsystem.
        memory::init_memory_subsystem(HEAP_START, HEAP_SIZE);

        // 3b. Reserve boot/kernel/heap memory in the frame allocator.
        // This has to happen right here, before *anything* else gets a
        // chance to call vmm_alloc_frame() -- it used to run much later
        // (after PCI scan, a demo allocation, and AHCI init), so all of
        // those were freely handing out frames from the "reserved" range.
        protect_boot_memory(&raw const _kernel_end as usize, HEAP_START, HEAP_SIZE);
        let _ = writeln!(uart, "[boot] 2: memory subsystem + protect_boot_memory done");

        // 3c. Bring the MMU up (AArch64 only)
        #[cfg(target_arch = "aarch64")]
        mmu::init(&mut uart);
        #[cfg(target_arch = "aarch64")]
        let _ = writeln!(uart, "[boot] 3: mmu::init() returned");

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

    // 1. MEMORY: flags demo
    let _code = MapFlags::kernel_code();
    let _data = MapFlags::kernel_data();

    // 2. GRAPHICS: Initialize the screen (x86_64 only)
    #[cfg(target_arch = "x86_64")]
    {
    const FB_ADDR: usize = 0xFD000000;
    const FB_WIDTH: usize = 1024;
    const FB_HEIGHT: usize = 768;
    const FB_PITCH: usize = 4096;

    let mut fb = unsafe {
        // Identity-map the framebuffer's MMIO pages before anything touches them
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

        Framebuffer::new(FB_ADDR, FB_WIDTH, FB_HEIGHT, FB_PITCH)
    };

    fb.clear(Color::BLACK);
    Framebuffer::draw_boot_splash(&mut fb);
    fb.draw_string(10, 70, "mitosOS System Init...", Color::GREEN);
    fb.draw_string(
        10, 90,
        if inited.is_some() { "Ramdisk: loaded" } else { "Ramdisk: missing" },
        Color::CYAN,
    );
    }

    // 3. HARDWARE: Start the timer.
    #[cfg(target_arch = "x86_64")]
    timer::hardware::init();

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
            let fat_adapter = alloc::sync::Arc::new(crate::fs::fat32_adapter::Fat32Adapter::new(fat_fs));
            crate::fs::vfs::VFS.lock().mount("/disk", fat_adapter);
        }
        Err(e) => {
            let _ = writeln!(uart, "mitosOS: FAT32 mount skipped ({e})");
        }
    }

    // --- FAT32 Mounting (real ATA disk) ---
    #[cfg(target_arch = "aarch64")]
    let block_device: Option<Box<dyn crate::block::BlockDevice>> = Some(Box::new(crate::block::RamBlockDevice::new(2048)));

    #[cfg(target_arch = "x86_64")]
    let block_device: Option<Box<dyn crate::block::BlockDevice>> = {
        match crate::fs::ata::AtaDevice::new() {
            Ok(mut ata_device) => {
                match ata_device.self_test() {
                    Ok(()) => {
                        let _ = writeln!(uart, "mitosOS: ATA self-test passed ({} sectors)", ata_device.total_sectors);
                    }
                    Err(e) => {
                        let _ = writeln!(uart, "mitosOS: ATA self-test FAILED: {e}");
                    }
                }
                Some(Box::new(ata_device))
            }
            Err(e) => {
                let _ = writeln!(uart, "mitosOS: Legacy ATA not found ({:?}), skipping.", e);
                None
            }
        }
    };

    if let Some(dev) = block_device {
        match crate::fs::fat32::Fat32FileSystem::mount(dev) {
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
    }

    // --- Spawn Background Worker Tasks ---
    crate::task::spawn(background_worker, crate::task::ExecutionMode::SharedThread, 0);
    crate::task::spawn(background_worker_2, crate::task::ExecutionMode::SharedThread, 0);

    // --- Start Kernel Shell ---
    shell::run(&mut uart, inited);
}

/// Background worker task demonstrating preemptive task execution
extern "C" fn background_worker() -> ! {
    loop {
        crate::task::yield_now();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut uart = uart::Uart::shared();
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

extern "C" fn background_worker_2() -> ! {
    loop {
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_str(&mut uart, "[Worker 2: Tick]\n");
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
        crate::task::yield_now();
    }
}
