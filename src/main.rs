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
// Limine boot protocol support -- see src/limine.rs.
#[cfg(target_arch = "x86_64")]
pub mod limine;
// Bootloader-agnostic layer over limine.rs + the Multiboot2 info
// structure -- see src/boot_info.rs.
#[cfg(target_arch = "x86_64")]
pub mod boot_info;

use core::fmt::Write;
use core::panic::PanicInfo;
use crate::memory::{protect_boot_memory, MapFlags};
#[cfg(target_arch = "x86_64")]
use crate::graphics::{Framebuffer, Color};
use alloc::boxed::Box;


const HEAP_START: usize = 0x150_000;

// x86_64's heap needs to stay within whatever the current boot
// protocol's identity/HHDM-style mapping actually covers -- at least
// the first 1GiB either way (Limine's HHDM covers considerably more;
// see memory::HHDM_OFFSET's doc comment) -- so 640KB starting at 1.3MB
// is comfortably conservative on any of them.
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

// _start (boot_x86.s) is reachable two ways on x86_64 -- Limine, and
// Multiboot2 (see boot_multiboot2.s) -- and forwards whatever it was
// entered with straight through into this call, untouched. Only a
// Multiboot2 boot puts anything meaningful there (the info pointer and
// the 0x36d76289 magic, relayed via boot_multiboot2.s); Limine
// guarantees every register is zero at entry, so both parameters are
// harmless noise on that path. aarch64 has no equivalent (Multiboot2/
// Limine are BIOS/UEFI-PC and UEFI concepts; the Raspberry Pi target
// boots through the SoC's own firmware instead), so it keeps the
// original no-argument signature.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn kmain(boot_arg0: u64, boot_arg1: u64) -> ! {
    // Must be the very first thing that runs, before even gdt::init()
    // a few lines into kmain_common: this is the only place that
    // calls memory::set_hhdm_offset, which nearly everything else in
    // the kernel eventually needs correct (via memory::phys_to_virt)
    // to dereference *any* physical address -- see that function's
    // doc comment, and boot_info::init's.
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

    // Checkpoint 0: kmain was reached at all, and the UART's own
    // register pokes (GPIO ALT function, baud divisor, FIFO enable)
    // didn't leave it unable to transmit.
    let _ = writeln!(uart, "[boot] 0: kmain reached, uart live");

    #[cfg(target_arch = "x86_64")]
    {
        // These all run long before graphics::WRITER exists (the
        // framebuffer isn't set up until much later in this
        // function) -- like every other pre-framebuffer boot
        // message, they go straight to the UART. println!/print!
        // target graphics::WRITER and would silently do nothing here
        // -- which is exactly what used to happen to this banner.
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
        // 1. Load the kernel's own GDT/TSS (ring-3 segments + the stack
        //    the CPU uses on any trap taken from ring 3). Must run before
        //    interrupts::init() -- the double-fault gate is set to use
        //    IST1, which this sets up.
        #[cfg(target_arch = "x86_64")]
        gdt::init();

        // 2. Install IDT/Vector table so the CPU can handle exceptions & IRQs.
        interrupts::init();
        let _ = writeln!(uart, "[boot] 1: interrupts::init() returned");

    
        
        #[cfg(target_arch = "x86_64")]
        hal::init();

        

        // 2b. Tear down PML4[0]'s temporary identity mapping now that a
        //     real IDT is live. On a Multiboot2 boot this is real: it
        //     removes the mapping boot_multiboot2.s's trampoline built
        //     to survive the paging-enable transition. On a Limine
        //     boot it's a harmless no-op -- base revision 3 (what this
        //     kernel requests) doesn't put anything at PML4[0] to begin
        //     with. See memory::unmap_low_half_identity_map's doc
        //     comment for why this has to run after interrupts::init(),
        //     not before it, either way.
        #[cfg(target_arch = "x86_64")]
        memory::unmap_low_half_identity_map();

        // 3. Initialize the heap allocator subsystem.
        memory::init_memory_subsystem(HEAP_START, HEAP_SIZE);

     #[cfg(target_arch = "x86_64")]
    {
        crate::hal::acpi::init();
    }


        // 3b. Reserve boot/kernel/heap memory in the frame allocator.
        // This has to happen right here, before *anything* else gets a
        // chance to call vmm_alloc_frame() -- it used to run much later
        // (after PCI scan, a demo allocation, and AHCI init), so all of
        // those were freely handing out frames from the "reserved" range.
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
        #[cfg(target_arch = "aarch64")]
        protect_boot_memory(0, kernel_end_addr, HEAP_START, HEAP_SIZE, None);
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
            // The ramdisk is loaded as a module -- Limine's
            // limine.conf `module_path`, or a Multiboot2 module tag --
            // rather than living at a fixed address; see boot_info::module().
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

    // The bootloader reports the real framebuffer geometry when it
    // finds one (boot_info::framebuffer()); the constants above are a
    // last-resort fallback for the unexpected case where it doesn't --
    // kept in case there's still a usable linear framebuffer at the
    // address a "-vga std"-style QEMU session conventionally uses.
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

// Use fb BEFORE moving it into Terminal.
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

// fb is MOVED here.
let terminal = graphics::Terminal::new(fb);

// Grab what's needed from `terminal` before handing it to WRITER
// below, which moves it.
let fb_width = terminal.fb.width;
let fb_height = terminal.fb.height;

// graphics::WRITER backs the println!/print! macros -- this was
// previously never set anywhere in the kernel, silently turning
// every println!/print! call (this crate-wide, including all of
// hal::acpi's diagnostics) into a no-op forever. From this point
// onward, use print!/println! instead of writing to `terminal`
// directly, so the rest of the kernel's life can actually use them
// too.
*graphics::WRITER.lock() = Some(terminal);

crate::print!("mitosOS Booting...\n");
crate::println!("Framebuffer resolution: {}x{}", fb_width, fb_height);

for i in 0..150 {
    crate::println!("Loading module {}...", i);
} 
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
