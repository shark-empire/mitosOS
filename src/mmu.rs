// AArch64 MMU bring-up.
#[cfg(target_arch = "aarch64")]


// x86_64 never needs a file like this: long mode *requires* paging, so
// the two-stage bootloader (stage1.s/stage2.s) already has the MMU on
// -- an identity map loaded into CR3 -- before kmain ever runs.
// AArch64's boot.s does nothing but set sp and jump straight to kmain.
// The MMU has been off this whole time, which means every
// `msr ttbr0_el1` in task::run_schedule and every
// `vmm::arch::map_page` call has had zero effect until now -- there
// was no translation happening for it to affect.

// This mirrors the "no higher-half split, physical == virtual"
// design already stated in memory.rs: everything (kernel now, user
// processes once task.rs spawns one) lives under a single
// TTBR0_EL1 root, with TTBR1_EL1 walks disabled entirely (TCR_EL1.EPD1)
// since nothing here uses a higher half.

use core::fmt::Write;
use crate::memory::{vmm_alloc_frame, MapFlags};
use crate::vmm::arch::{map_page, PageTable};

/// How much low physical RAM to identity-map at boot, starting at 0x0.
/// Comfortably covers the kernel image (linked at 0x80000), the heap
/// (0x150000..0x1F0000), and headroom for page tables/ELF
/// segments/user stacks the PMM hands out later. For reference,
/// x86_64's own bootloader-built identity map is ~4MiB (see the note
/// in elf.rs); this is deliberately more generous. The PMM's own
/// ceiling is 256MiB (BitmapAllocator<1024>) -- if this kernel ever
/// actually allocates anywhere near that much, bump this constant to
/// match; mapping the full 256MiB up front works too, it's just ~65k
/// redundant map_page calls at boot for headroom nothing uses yet.
const KERNEL_IDENTITY_MAP_SIZE: usize = 32 * 1024 * 1024; // 32MiB

const PAGE_SIZE: usize = 4096;

/// QA7 ARM-local interrupt controller -- see
/// interrupts::init_gic_timer_irq (raspi3b/BCM2837 has no real GIC;
/// this is what actually routes the generic timer IRQ to a core).
/// One page is enough; CORE0_TIMER_IRQCNTL lives at offset 0x40.
/// interrupts::init() (and this write) runs before this MMU init, so
/// nothing today reads or writes this range post-MMU-enable -- mapped
/// anyway so a later addition here doesn't inherit a stale mapping.
const LOCAL_BASE: usize = 0x4000_0000;

/// GPIO + UART0 (PL011) -- see uart.rs's aarch64 `imp` module. One page
/// covers GPFSEL1/GPPUD/GPPUDCLK0 (GPIO_BASE); a second, adjacent page
/// covers UART0's own registers (UART0_BASE = GPIO_BASE + 0x1000).
const GPIO_BASE: usize = 0x3F20_0000;
const UART0_BASE: usize = 0x3F20_1000;

/// Matches FB_ADDR/FB_WIDTH/FB_HEIGHT/FB_PITCH in main.rs. Kept as a
/// separate constant here rather than importing main's so this module
/// doesn't depend on main.rs's internals -- bump both places together
/// if the framebuffer geometry ever changes.
const FB_ADDR: usize = 0xFD00_0000;
const FB_SIZE: usize = 4096 * 768; // FB_PITCH * FB_HEIGHT

/// The kernel's own top-level table. Every process's page table
/// (memory::create_process_page_table) is a byte-for-byte copy of
/// this one made *after* init() has populated it, so any mapping
/// made here before a process exists is visible to all of them --
/// the same property x86_64's PML4-copy already relies on.
static mut KERNEL_ROOT: usize = 0;

/// Identity-maps `size` bytes starting at `base`. AlreadyMapped errors
/// are swallowed deliberately: none of the ranges below actually
/// overlap today, but re-asserting an existing identical mapping
/// would be harmless if a future constant change ever made them.
unsafe fn identity_map_range(root: *mut PageTable, base: usize, size: usize, flags: MapFlags) {
    let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    for i in 0..pages {
        let addr = base + i * PAGE_SIZE;
        unsafe {
            let _ = map_page(root, addr, addr, flags);
        }
    }
}

/// Reads ID_AA64MMFR0_EL1.PARange (bits 3:0) -- the physical address
/// size this specific core actually supports -- rather than guessing
/// a fixed IPS encoding for TCR_EL1. The field's encoding already
/// matches TCR_EL1.IPS's encoding 1:1, so it's used directly.
unsafe fn read_pa_range_bits() -> u64 {
    let mmfr0: u64;
    unsafe {
        core::arch::asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack));
    }
    mmfr0 & 0xF
}

/// Brings the AArch64 MMU up: builds the kernel's identity-mapped
/// TTBR0_EL1 table, loads MAIR_EL1/TCR_EL1, and sets SCTLR_EL1.M.
///
/// Must run after the physical frame allocator is usable
/// (protect_boot_memory) and before anything else in kmain touches
/// the QA7 local controller or UART MMIO again -- interrupts::init()
/// already ran its one-time local-controller setup before this,
/// pre-MMU, so that's unaffected; what matters is everything *after*
/// this call.
///
/// # Safety
/// Must be called exactly once, from core 0, this early in boot.
pub unsafe fn init(uart: &mut crate::uart::Uart) {
    let _ = writeln!(uart, "mitosOS: MMU bring-up starting...");

    let root_frame = match vmm_alloc_frame() {
        Some(f) => f,
        None => {
            let _ = writeln!(uart, "mitosOS: MMU init FAILED -- out of memory for the root table.");
            loop { core::hint::spin_loop(); }
        }
    };
    unsafe { core::ptr::write_bytes(root_frame as *mut u8, 0, PAGE_SIZE); }
    let root = root_frame as *mut PageTable;

    let normal = MapFlags { writable: true, user_accessible: false, execute_disable: false, device: false };
    let device = MapFlags { writable: true, user_accessible: false, execute_disable: true, device: true };

    unsafe {
        identity_map_range(root, 0x0, KERNEL_IDENTITY_MAP_SIZE, normal);
        identity_map_range(root, LOCAL_BASE, PAGE_SIZE, device);
        identity_map_range(root, GPIO_BASE, PAGE_SIZE, device);
        identity_map_range(root, UART0_BASE, PAGE_SIZE, device);
        identity_map_range(root, FB_ADDR, FB_SIZE, device);
    }

    let _ = writeln!(
        uart,
        "mitosOS: MMU identity map built ({} MiB kernel RAM + MMIO). Enabling...",
        KERNEL_IDENTITY_MAP_SIZE / (1024 * 1024)
    );

    unsafe {
        KERNEL_ROOT = root_frame;

        // MAIR_EL1: index 0 = Normal, Write-Back R/W-Allocate (0xFF).
        // index 1 = Device-nGnRnE (0x00).
        let mair: u64 = 0x0000_0000_0000_00FF;

        // TCR_EL1: 4KB granule, 48-bit input address space via TTBR0
        // only. T1SZ/TG1/IRGN1/ORGN1/SH1 are set for consistency but
        // never consulted -- EPD1 disables TTBR1 walks entirely,
        // since nothing here uses a higher half.
        let ips = read_pa_range_bits();
        let tcr: u64 = (16u64)          // T0SZ = 16 -> 2^48 byte input space
            | (0b01u64 << 8)            // IRGN0: Normal WB R/W-Allocate
            | (0b01u64 << 10)           // ORGN0: Normal WB R/W-Allocate
            | (0b11u64 << 12)           // SH0: Inner Shareable
            | (0b00u64 << 14)           // TG0: 4KB granule
            | (16u64 << 16)             // T1SZ = 16 (unused, EPD1=1)
            | (1u64 << 23)              // EPD1: disable TTBR1 walks
            | (0b10u64 << 30)           // TG1: 4KB granule (unused, EPD1=1)
            | (ips << 32);              // IPS: from ID_AA64MMFR0_EL1.PARange

        core::arch::asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {root}",
            "isb",
            // Discard whatever the TLB may hold from before the MMU
            // was on -- architecturally unpredictable, not guaranteed
            // empty, so this isn't optional.
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            mair = in(reg) mair,
            tcr = in(reg) tcr,
            root = in(reg) root_frame as u64,
            options(nostack),
        );

        // Read-modify-write, not a fixed constant -- some SCTLR_EL1
        // bits are RES1 depending on the core, and blindly writing a
        // hardcoded value risks clearing one of them.
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
        sctlr |= 1 << 0;  // M: MMU enable
        sctlr |= 1 << 2;  // C: data cache enable
        sctlr |= 1 << 12; // I: instruction cache enable
        core::arch::asm!(
            "msr sctlr_el1, {0}",
            "isb",
            in(reg) sctlr,
            options(nostack),
        );
    }

    let _ = writeln!(uart, "mitosOS: MMU enabled (TTBR0_EL1 = 0x{:x}).", root_frame);
}

/// The kernel's own page table root -- every process table is cloned
/// from this one. See memory::create_process_page_table.
pub fn kernel_root() -> usize {
    unsafe { KERNEL_ROOT }
}
