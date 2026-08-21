// Repo path: src/main.rs
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

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
#[cfg(target_arch = "x86_64")]
pub mod limine;
#[cfg(target_arch = "x86_64")]
pub mod boot_info;

use core::fmt::Write;
use core::panic::PanicInfo;
use crate::memory::{protect_boot_memory, MapFlags};
#[cfg(target_arch = "x86_64")]
use crate::graphics::{Framebuffer, Color};
use alloc::boxed::Box;

const HEAP_START: usize = 0x150_000;

#[cfg(target_arch = "x86_64")]
const HEAP_SIZE: usize = 0xA0_000; // 640KB

#[cfg(target_arch = "aarch64")]
const HEAP_SIZE: usize = 0x800_000; // 8MB

unsafe extern "C" {
    static _kernel_end: u8;
}

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn kmain(boot_arg0: u64, boot_arg1: u64) -> ! {
    boot_info::init(boot_arg0, boot_arg1);
    kmain_common()
}

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn kmain() -> ! {
    kmain_common()
}

fn kmain_common() -> ! {
    let mut uart = unsafe { uart::Uart::init() };

    let _ = writeln!(uart, "[boot] 0: kmain reached, uart live");

    #[cfg(target_arch = "x86_64")]
    {
        if crate::limine::detected() {
            let _ = writeln!(uart, "mitosOS: booted via Limine");
        } else {
            let _ = writeln!(uart, "mitosOS: WARN no supported boot protocol detected");
        }
        if !crate::limine::base_revision_supported() {
            let _ = writeln!(
                uart,
                "mitosOS: WARN Limine did not grant the exact requested base revision"
            );
        }
        if let Some((entries, usable_bytes)) = boot_info::memmap_summary() {
            let _ = writeln!(
                uart,
                "mitosOS: bootloader memory map: {entries} entries, {} KiB usable",
                usable_bytes / 1024
            );
        }
    }

    unsafe {
        // 1. Load CPU GDT & TSS
        #[cfg(target_arch = "x86_64")]
        gdt::init();

        // 2. Install IDT / Interrupt vector handlers
        interrupts::init();
        let _ = writeln!(uart, "[boot] 1: interrupts::init() returned");

        // 3. Initialize Heap Allocator Subsystem
        memory::init_memory_subsystem(HEAP_START, HEAP_SIZE);

        // 3a. Initialize Physical Memory Manager from Limine's Memory Map
        #[cfg(target_arch = "x86_64")]
        if let Err(e) = memory::init_pmm_from_limine() {
            let _ = writeln!(uart, "mitosOS: WARN init_pmm_from_limine failed: {e}");
        }

        // 3b. Reserve Kernel, Heap, and Ramdisk ranges in physical memory
        let kernel_end_addr = &raw const _kernel_end as usize;
        #[cfg(target_arch = "x86_64")]
        {
            let kernel_phys_start = boot_info::kernel_phys_start();
            let kernel_phys_end = kernel_phys_start + (kernel_end_addr - boot_info::KERNEL_VMA);
            protect_boot_memory(
                kernel_phys_start,
                kernel_phys_end,
                HEAP_START,
                HEAP_SIZE,
                boot_info::module(),
            );
        }

        // 3a (aarch64). No bootloader memmap to parse here -- free the PMM's
        // trackable range as usable RAM before anything tries to allocate
        // from it. See init_pmm_static's doc comment.
        #[cfg(target_arch = "aarch64")]
        memory::init_pmm_static();

        #[cfg(target_arch = "aarch64")]
        protect_boot_memory(0, kernel_end_addr, HEAP_START, HEAP_SIZE, None);
        let _ = writeln!(uart, "[boot] 2: memory subsystem + protect_boot_memory done");

        // 4. Initialize Hardware Abstraction Layer & Parse ACPI
        #[cfg(target_arch = "x86_64")]
        hal::init();

        // 4b. Reclaim temporary bootloader & ACPI memory regions back into PMM
        #[cfg(target_arch = "x86_64")]
        memory::reclaim_boot_memory();

        // 5. Tear down lower half identity map
        #[cfg(target_arch = "x86_64")]
        memory::unmap_low_half_identity_map();

        // Bring MMU up (AArch64 only)
        #[cfg(target_arch = "aarch64")]
        mmu::init(&mut uart);
        #[cfg(target_arch = "aarch64")]
        let _ = writeln!(uart, "[boot] 3: mmu::init() returned");

        // Unmask UART interrupts and enable CPU-level interrupts
        uart.enable_interrupts();
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
            
            if dev.class == 0x01 && dev.subclass == 0x06 {
                let _ = writeln!(uart, ">>> FOUND AHCI CONTROLLER! BAR5 Address: 0x{:X} <<<", dev.bar5);
            }
        }
        let _ = writeln!(uart, "-------------------------");
    }

    if let Some(frame) = crate::memory::alloc_frame() {
        let _ = writeln!(uart, "Memory Manager: Allocated physical frame at 0x{:X}", frame);
    }

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
            match boot_info::module() {
                Some((addr, size)) => unsafe { ramdisk::TarFileSystem::new(addr, size) },
                None => {
                    let _ = writeln!(
                        uart,
                        "mitosOS: WARN no ramdisk module from bootloader (check limine.conf's module_path)"
                    );
                    None
                }
            }
        }
    };

    if let Some(tar_fs) = inited {
        let adapter = alloc::sync::Arc::new(crate::fs::tar_adapter::TarFsAdapter::new(tar_fs));
        crate::fs::vfs::VFS.lock().mount("/", adapter);
        let _ = writeln!(uart, "mitosOS: initrd detected and VFS mounted at /");
    } else {
        let _ = writeln!(uart, "mitosOS: WARN - No valid initrd found.");
    }

    let _code = MapFlags::kernel_code();
    let _data = MapFlags::kernel_data();

    // GRAPHICS: Initialize Framebuffer
    #[cfg(target_arch = "x86_64")]
    {
        const FB_ADDR: usize = 0xFD000000;
        const FB_WIDTH: usize = 1024;
        const FB_HEIGHT: usize = 768;
        const FB_PITCH: usize = 4096;

        let reported_fb = boot_info::framebuffer();
        let (addr, width, height, pitch, needs_mapping) =
            reported_fb.unwrap_or((FB_ADDR, FB_WIDTH, FB_HEIGHT, FB_PITCH, true));

        let mut fb = unsafe {
            if reported_fb.is_none() {
                let _ = writeln!(
                    uart,
                    "mitosOS: WARN no framebuffer from bootloader, trying fallback address"
                );
            }

            if needs_mapping {
                let cr3: usize;
                core::arch::asm!(
                    "mov {}, cr3",
                    out(reg) cr3,
                    options(nomem, nostack)
                );

                let page_table_root = cr3 & !0xFFF;
                let fb_pages = (pitch * height + 0xFFF) / 0x1000;

                for i in 0..fb_pages {
                    let page_addr = addr + i * 0x1000;

                    if let Err(e) = crate::memory::map_page(
                        page_table_root,
                        page_addr,
                        page_addr,
                    ) {
                        let _ = writeln!(
                            uart,
                            "mitosOS: WARN framebuffer mapping failed: {e}"
                        );
                        break;
                    }
                }
            }

            Framebuffer::new(addr, width, height, pitch)
        };

        fb.clear(Color::BLACK);
        Framebuffer::draw_boot_splash(&mut fb);

        fb.draw_string(
            10,
            70,
            "mitosOS System Init...",
            Color::GREEN,
        );

        fb.draw_string(
            10,
            90,
            if inited.is_some() {
                "Ramdisk: loaded"
            } else {
                "Ramdisk: missing"
            },
            Color::CYAN,
        );

        let terminal = graphics::Terminal::new(fb);
        let fb_width = terminal.fb.width;
        let fb_height = terminal.fb.height;

        *graphics::WRITER.lock() = Some(terminal);

        crate::print!("mitosOS Booting...\n");
        crate::println!("Framebuffer resolution: {}x{}", fb_width, fb_height);

        for i in 0..150 {
            crate::println!("Loading module {}...", i);
        } 
    } 

    // HARDWARE: Start Timer
    #[cfg(target_arch = "x86_64")]
    timer::hardware::init();

    // FAT32 Mounting (RAM-backed)
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

    // FAT32 Mounting (ATA disk)
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

    if let Some(mut dev) = block_device {
        // Real disks (and QEMU's vvfat `if=ide` driver) present an MBR
        // partition table at LBA 0, not a bare FAT boot sector -- see
        // fs::mbr's doc comment for why mounting directly at LBA 0
        // instead produced "Unsupported sector size" here. Probe for
        // one first so the FAT driver gets pointed at the actual
        // volume.
        let mount_result = match crate::fs::mbr::find_first_partition_lba(&mut *dev) {
            Ok(Some(lba)) => {
                let _ = writeln!(
                    uart,
                    "mitosOS: MBR found on ATA disk, first partition at LBA {lba}"
                );
                let partitioned: Box<dyn block::BlockDevice> =
                    Box::new(block::PartitionBlockDevice::new(dev, lba as usize));
                crate::fs::fat32::Fat32FileSystem::mount(partitioned)
            }
            Ok(None) => crate::fs::fat32::Fat32FileSystem::mount(dev),
            Err(e) => {
                let _ = writeln!(
                    uart,
                    "mitosOS: WARN MBR probe failed ({e}), trying LBA 0 directly"
                );
                crate::fs::fat32::Fat32FileSystem::mount(dev)
            }
        };

        match mount_result {
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

    // Spawn Background Tasks & Shell
    crate::task::spawn(background_worker, crate::task::ExecutionMode::SharedThread, 0);
    crate::task::spawn(background_worker_2, crate::task::ExecutionMode::SharedThread, 0);

    shell::run(&mut uart, inited);
}

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
    let mut uart = crate::uart::Uart::shared();
    
    for _ in 0..5 {
        let _ = core::fmt::Write::write_str(&mut uart, "[Worker 2: Tick]\n");
        crate::task::yield_now();
    }

    loop {
        crate::task::yield_now();
    }
}
