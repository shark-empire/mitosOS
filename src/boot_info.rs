//! Bootloader-agnostic boot info (x86_64 only).
//!
//! `kmain` can be reached two ways on x86_64: natively via Limine
//! (src/limine.rs), or via any Multiboot2 loader (src/boot_multiboot2.s).
//! This module figures out which one actually happened and normalises
//! whatever boot information each provides -- a framebuffer, a ramdisk
//! module, a memory map summary, where the kernel itself physically
//! is -- into one shape the rest of main.rs can use without caring
//! which protocol it came from.
//!
//! Detection: `boot_x86.s`'s `_start` forwards whatever it was
//! entered with straight through into `call kmain` untouched (see the
//! comment at its top). For Multiboot2, that means the info pointer
//! and the 0x36d76289 magic end up in kmain's two parameters (relayed
//! there by boot_multiboot2.s -- see its file header); for Limine,
//! both parameters are guaranteed zero (Limine clears every GPR
//! before jumping to the entry point -- PROTOCOL.md, "Machine State
//! at Entry").
//!
//! `init()` must be the very first thing kmain does, before even
//! gdt::init(). It's not just about this module's own state: it's
//! also the only place that calls `memory::set_hhdm_offset`, which
//! nearly everything else in the kernel eventually needs correct
//! (via `memory::phys_to_virt`) to dereference *any* physical address
//! -- see that function's doc comment.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

const ORDER: Ordering = Ordering::SeqCst;

/// Where the kernel is linked (see linker_x86.ld's KERNEL_VMA, which
/// this must match) -- combined with `kernel_phys_start()`, gives the
/// kernel's own physical extent for `memory::protect_boot_memory`.
pub const KERNEL_VMA: usize = 0xffffffff80000000;

/// Multiboot2's magic, checked in kmain (and, initially, in
/// boot_multiboot2.s's 32-bit entry): if the loader put this in EAX
/// at entry, we're Multiboot2-booted, no matter what else is true.
const MULTIBOOT2_MAGIC: u64 = 0x36d76289;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BootProtocol {
    Unknown = 0,
    Limine = 1,
    Multiboot2 = 2,
}

static PROTOCOL: AtomicU8 = AtomicU8::new(BootProtocol::Unknown as u8);

static FB_ADDR: AtomicU64 = AtomicU64::new(0);
static FB_WIDTH: AtomicU64 = AtomicU64::new(0);
static FB_HEIGHT: AtomicU64 = AtomicU64::new(0);
static FB_PITCH: AtomicU64 = AtomicU64::new(0);
// Multiboot2's framebuffer address, unlike Limine's, is not guaranteed
// to already be mapped -- see the note on `framebuffer()` below.
static FB_NEEDS_MAPPING: AtomicU8 = AtomicU8::new(0);

static MOD_ADDR: AtomicU64 = AtomicU64::new(0);
static MOD_SIZE: AtomicU64 = AtomicU64::new(0);

static MEMMAP_ENTRIES: AtomicU64 = AtomicU64::new(0);
static MEMMAP_USABLE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Physical address the kernel image itself starts at. Defaults to
/// KERNEL_LMA_BASE from linker_x86.ld (1MiB) -- correct as-is for a
/// Multiboot2 boot, and overridden for Limine (which picks its own
/// physical placement -- PROTOCOL.md, "Executable Address" feature).
static KERNEL_PHYS_START: AtomicU64 = AtomicU64::new(0x0010_0000);

/// Must be called exactly once, at the very top of kmain -- see the
/// module doc comment for why.
pub fn init(arg0: u64, arg1: u64) {
    if arg1 == MULTIBOOT2_MAGIC {
        PROTOCOL.store(BootProtocol::Multiboot2 as u8, ORDER);
        // memory::HHDM_OFFSET's default already matches what
        // boot_multiboot2.s's trampoline built (see its comments) --
        // nothing to override here, unlike the Limine branch below.
        unsafe { parse_multiboot2(arg0 as usize) };
        return;
    }

    // `detected()` (any Limine request actually answered) rather than
    // `base_revision_supported()` (that exact revision granted): the
    // two used to be conflated here, so a bootloader that answers
    // every request but falls back to an older base revision than
    // requested (a real, spec-legal outcome, not a "this isn't
    // Limine" signal) took the "unknown bootloader" branch below --
    // silently skipping set_hhdm_offset for the rest of boot. main.rs
    // logs separately if the exact revision wasn't granted.
    if crate::limine::detected() {
        PROTOCOL.store(BootProtocol::Limine as u8, ORDER);

        if let Some(offset) = crate::limine::hhdm_offset() {
            crate::memory::set_hhdm_offset(offset as usize);
        }
        // else: no HHDM response -- extremely unlikely for a
        // spec-conforming Limine, but if it happens, every physical
        // access falls back to memory::HHDM_OFFSET's compiled-in
        // default, which is *not* guaranteed correct for a Limine
        // boot. There is no better fallback available.

        if let Some((phys_base, _virt_base)) = crate::limine::executable_address() {
            KERNEL_PHYS_START.store(phys_base, ORDER);
        }

        if let Some((addr, w, h, pitch)) = crate::limine::framebuffer() {
            FB_ADDR.store(addr as u64, ORDER);
            FB_WIDTH.store(w as u64, ORDER);
            FB_HEIGHT.store(h as u64, ORDER);
            FB_PITCH.store(pitch as u64, ORDER);
            // Limine hands this back already mapped (HHDM), unlike
            // Multiboot2's -- see limine.rs::framebuffer().
            FB_NEEDS_MAPPING.store(0, ORDER);
        }
        if let Some((addr, size)) = crate::limine::first_module() {
            MOD_ADDR.store(addr as u64, ORDER);
            MOD_SIZE.store(size as u64, ORDER);
        }
        if let Some((count, bytes)) = crate::limine::memmap_summary() {
            MEMMAP_ENTRIES.store(count as u64, ORDER);
            MEMMAP_USABLE_BYTES.store(bytes, ORDER);
        }
        return;
    }

    // Reachable only if a bootloader other than Limine or Multiboot2
    // somehow jumped to _start directly -- not a configuration this
    // kernel supports building for anymore, but fails soft rather
    // than assuming Limine and silently using an unconfirmed HHDM
    // offset.
    PROTOCOL.store(BootProtocol::Unknown as u8, ORDER);
}

pub fn protocol() -> BootProtocol {
    match PROTOCOL.load(ORDER) {
        1 => BootProtocol::Limine,
        2 => BootProtocol::Multiboot2,
        _ => BootProtocol::Unknown,
    }
}

pub fn protocol_name() -> &'static str {
    match protocol() {
        BootProtocol::Limine => "Limine",
        BootProtocol::Multiboot2 => "Multiboot2",
        BootProtocol::Unknown => "unknown bootloader",
    }
}

/// (address, width, height, pitch, needs_identity_mapping).
///
/// `needs_identity_mapping` is true only for a Multiboot2 boot: the
/// address GRUB (or Limine acting as a Multiboot2 loader) reports is
/// wherever the GPU's framebuffer BAR actually sits, commonly well
/// above the 1GiB the boot trampoline identity-maps, and unlike
/// Limine's response it is not guaranteed mapped at all -- the caller
/// must map it before touching it, exactly as main.rs does for it.
pub fn framebuffer() -> Option<(usize, usize, usize, usize, bool)> {
    let addr = FB_ADDR.load(ORDER);
    if addr == 0 {
        return None;
    }
    Some((
        addr as usize,
        FB_WIDTH.load(ORDER) as usize,
        FB_HEIGHT.load(ORDER) as usize,
        FB_PITCH.load(ORDER) as usize,
        FB_NEEDS_MAPPING.load(ORDER) != 0,
    ))
}

/// (address, size) of the ramdisk module, however it was passed in
/// (Limine's `module_path` / limine.conf, or a Multiboot2 module
/// tag). For Multiboot2, this address is only valid through the boot
/// trampoline's identity mapping (mirrored at the offset
/// memory::phys_to_virt uses by default -- see boot_multiboot2.s),
/// same caveat as the framebuffer above.
pub fn module() -> Option<(usize, usize)> {
    let addr = MOD_ADDR.load(ORDER);
    if addr == 0 {
        return None;
    }
    Some((addr as usize, MOD_SIZE.load(ORDER) as usize))
}

/// (entry count, total usable bytes), if the bootloader provided a
/// memory map. Diagnostic only for now -- see limine.rs::memmap_summary().
pub fn memmap_summary() -> Option<(usize, u64)> {
    let entries = MEMMAP_ENTRIES.load(ORDER);
    if entries == 0 {
        return None;
    }
    Some((entries as usize, MEMMAP_USABLE_BYTES.load(ORDER)))
}

/// Physical address the kernel image starts at -- see
/// `memory::protect_boot_memory`'s doc comment for why this can't be
/// assumed fixed on a Limine boot.
pub fn kernel_phys_start() -> usize {
    KERNEL_PHYS_START.load(ORDER) as usize
}

// --- Multiboot2 info structure parsing -----------------------------------
//
// Deliberately not modelled as Rust structs: Multiboot2 only promises
// each *tag* is 8-byte aligned as a whole, not that every field inside
// it lands on a naturally-aligned offset for its type, so fields are
// read individually via `read_unaligned` at explicit byte offsets
// instead of through a `#[repr(C)]` struct reference. Tag layouts
// below are from the (long-stable, unchanged) Multiboot2 specification.

const MB2_TAG_END: u32 = 0;
const MB2_TAG_MODULE: u32 = 3;
const MB2_TAG_MMAP: u32 = 6;
const MB2_TAG_FRAMEBUFFER: u32 = 8;
const MB2_MEMORY_AVAILABLE: u32 = 1;

/// Info structures this small are what every sane loader produces;
/// this is just a guard against walking off into unmapped memory if
/// `total_size` were ever garbage.
const MB2_MAX_INFO_SIZE: usize = 64 * 1024;

unsafe fn read_u32(base: *const u8, offset: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(base.add(offset) as *const u32) }
}

unsafe fn read_u64(base: *const u8, offset: usize) -> u64 {
    unsafe { core::ptr::read_unaligned(base.add(offset) as *const u64) }
}

/// # Safety
/// `info_phys` must be the physical address Multiboot2 handed the
/// kernel in EBX at entry (relayed here via RDI -- see the module doc
/// comment), reachable through whatever mapping
/// `memory::HHDM_OFFSET`'s current value provides (boot_multiboot2.s's
/// trampoline builds one covering it, matching that default -- see
/// its comments).
unsafe fn parse_multiboot2(info_phys: usize) {
    if info_phys == 0 {
        return;
    }
    let base = crate::memory::phys_to_virt(info_phys) as *const u8;
    let total_size = (unsafe { read_u32(base, 0) } as usize).min(MB2_MAX_INFO_SIZE);

    let mut offset = 8usize; // total_size(4) + reserved(4)
    let mut memmap_entries = 0u64;
    let mut memmap_usable = 0u64;

    while offset + 8 <= total_size {
        let tag_type = unsafe { read_u32(base, offset) };
        let tag_size = unsafe { read_u32(base, offset + 4) } as usize;
        if tag_type == MB2_TAG_END || tag_size < 8 {
            break;
        }

        match tag_type {
            MB2_TAG_FRAMEBUFFER if tag_size >= 24 => {
                let addr = unsafe { read_u64(base, offset + 8) };
                let pitch = unsafe { read_u32(base, offset + 16) };
                let width = unsafe { read_u32(base, offset + 20) };
                let height = unsafe { read_u32(base, offset + 24) };
                FB_ADDR.store(addr, ORDER);
                FB_WIDTH.store(width as u64, ORDER);
                FB_HEIGHT.store(height as u64, ORDER);
                FB_PITCH.store(pitch as u64, ORDER);
                FB_NEEDS_MAPPING.store(1, ORDER);
            }
            MB2_TAG_MODULE if tag_size >= 16 => {
                // Only the first module is used (the ramdisk).
                if MOD_ADDR.load(ORDER) == 0 {
                    let mod_start = unsafe { read_u32(base, offset + 8) };
                    let mod_end = unsafe { read_u32(base, offset + 12) };
                    if mod_end > mod_start {
                        MOD_ADDR.store(mod_start as u64, ORDER);
                        MOD_SIZE.store((mod_end - mod_start) as u64, ORDER);
                    }
                }
            }
            MB2_TAG_MMAP if tag_size >= 16 => {
                let entry_size = (unsafe { read_u32(base, offset + 8) } as usize).max(1);
                let entries_end = offset + tag_size;
                let mut eoff = offset + 16;
                while eoff + 24 <= entries_end {
                    let len = unsafe { read_u64(base, eoff + 8) };
                    let etype = unsafe { read_u32(base, eoff + 16) };
                    if etype == MB2_MEMORY_AVAILABLE {
                        memmap_usable += len;
                    }
                    memmap_entries += 1;
                    eoff += entry_size;
                }
            }
            _ => {}
        }

        offset += (tag_size + 7) & !7; // tags are 8-byte aligned
    }

    if memmap_entries > 0 {
        MEMMAP_ENTRIES.store(memmap_entries, ORDER);
        MEMMAP_USABLE_BYTES.store(memmap_usable, ORDER);
    }
}
