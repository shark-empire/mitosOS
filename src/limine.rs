//! Limine boot protocol support (x86_64 only).
//!
//! This is a small, hand-written implementation of just the requests
//! mitosOS actually uses, built directly from limine-protocol's
//! PROTOCOL.md (the spec Limine v12.x implements), rather than a
//! vendored `limine.h` or the `limine` crate -- so there's exactly one
//! source of truth for these numbers and no extra build dependency.
//! Every ID and struct layout below is copied verbatim from the spec;
//! see https://github.com/Limine-Bootloader/limine-protocol/blob/trunk/PROTOCOL.md
//! for the full reference if you need to add more requests later --
//! RSDP is the most likely next one, and follows the exact same
//! request/response shape as the ones below.
//!
//! How this gets used: Limine scans the loaded kernel image for
//! these statics (by their magic `id`) and, for each one it
//! recognises, writes a pointer into that request's `response` field
//! before jumping to the kernel's entry point. If a request's
//! `response` is still NULL by the time kmain runs, Limine didn't
//! support or fulfil it -- callers here already handle that as "not
//! available" rather than assuming success.
//!
//! This module only *declares* the requests and lets Limine fill
//! them in; the functions below (`framebuffer()`, `first_module()`,
//! etc.) are what main.rs actually calls to read them back.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

const COMMON_MAGIC_0: u64 = 0xc7b1dd30df4c8b88;
const COMMON_MAGIC_1: u64 = 0x0a82e883a194f07b;

// --- Base revision tag -----------------------------------------------------
//
// Requesting revision 3: the highest revision every bootloader that
// supports base revision 3 or later is *guaranteed* to fall back to
// even if it doesn't recognise a newer one (PROTOCOL.md, "Base
// Revisions"). Every x86-64-relevant guarantee this kernel relies on
// (restrictive HHDM, the modern memory map layout) is already in
// place as of revision 3; revisions 4-6 add guarantees that are
// either not used here (ACPI-region HHDM mapping) or aarch64/riscv64
// /loongarch64-only. Bumping this later is a one-line change.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: [AtomicU64; 3] = [
    AtomicU64::new(0xf9562b2d5c95a6c8),
    AtomicU64::new(0x6a7b384944536bdc),
    AtomicU64::new(3),
];

/// True if Limine reported it loaded us with the base revision we
/// asked for (its 3rd component is left as-is, non-zero, otherwise).
pub fn base_revision_supported() -> bool {
    BASE_REVISION[2].load(Ordering::SeqCst) == 0
}

// --- HHDM (Higher Half Direct Map) ---------------------------------------
//
// The offset main.rs feeds into memory::set_hhdm_offset -- see that
// function's doc comment for why every other physical-address
// dereference in the kernel depends on this being read (for a Limine
// boot) before anything else runs.

#[repr(C)]
struct HhdmRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<HhdmResponse>,
}

#[repr(C)]
struct HhdmResponse {
    revision: u64,
    offset: u64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest {
    id: [COMMON_MAGIC_0, COMMON_MAGIC_1, 0x48dcf1cb8ad2b852, 0x63984e959a98244b],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// The offset of Limine's Higher Half Direct Map. Bootloader-chosen
/// and not guaranteed stable across boots (PROTOCOL.md is explicit
/// that it "may vary... including for randomisation") -- never assume
/// a fixed value, always read this.
pub fn hhdm_offset() -> Option<u64> {
    let resp = HHDM_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    Some(unsafe { &*resp }.offset)
}

// --- Executable Address ----------------------------------------------------
//
// Where the kernel itself actually ended up physically -- Limine picks
// this itself (PROTOCOL.md: "No specific physical memory placement is
// guaranteed"), so, unlike a Multiboot2 boot (always 1MiB -- see
// KERNEL_LMA_BASE in linker_x86.ld), it has to be queried rather than
// assumed. Used to correctly reserve the kernel's own memory in the
// physical frame allocator -- see memory::protect_boot_memory's doc
// comment.

#[repr(C)]
struct ExecutableAddressRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<ExecutableAddressResponse>,
}

#[repr(C)]
struct ExecutableAddressResponse {
    revision: u64,
    physical_base: u64,
    virtual_base: u64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static EXECUTABLE_ADDRESS_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest {
    id: [COMMON_MAGIC_0, COMMON_MAGIC_1, 0x71ba76863cc55f63, 0xb2644a48c516a487],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// (physical_base, virtual_base) of the loaded kernel image.
pub fn executable_address() -> Option<(u64, u64)> {
    let resp = EXECUTABLE_ADDRESS_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    let resp = unsafe { &*resp };
    Some((resp.physical_base, resp.virtual_base))
}

// --- Framebuffer -------------------------------------------------------

#[repr(C)]
struct FramebufferRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<FramebufferResponse>,
}

#[repr(C)]
struct FramebufferResponse {
    revision: u64,
    framebuffer_count: u64,
    framebuffers: *const *const Framebuffer,
}

#[repr(C)]
struct Framebuffer {
    address: *mut u8,
    width: u64,
    height: u64,
    pitch: u64,
    bpp: u16,
    memory_model: u8,
    red_mask_size: u8,
    red_mask_shift: u8,
    green_mask_size: u8,
    green_mask_shift: u8,
    blue_mask_size: u8,
    blue_mask_shift: u8,
    unused: [u8; 7],
    edid_size: u64,
    edid: *const u8,
    mode_count: u64,
    modes: *const *const u8,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest {
    id: [COMMON_MAGIC_0, COMMON_MAGIC_1, 0x9d5827dcd881dd75, 0xa3148604f6fab11b],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// (address, width, height, pitch). `address` is already a usable
/// (HHDM) virtual pointer -- Limine maps it before handoff, no
/// identity-mapping step needed, unlike the legacy bootloader path.
pub fn framebuffer() -> Option<(usize, usize, usize, usize)> {
    let resp = FRAMEBUFFER_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    // SAFETY: non-null response pointers are guaranteed valid by the
    // protocol once the bootloader has set them.
    let resp = unsafe { &*resp };
    if resp.framebuffer_count == 0 || resp.framebuffers.is_null() {
        return None;
    }
    // SAFETY: `framebuffers` points to `framebuffer_count` pointers,
    // per the response's contract; we only ever read the first.
    let fb_ptr = unsafe { *resp.framebuffers };
    if fb_ptr.is_null() {
        return None;
    }
    let fb = unsafe { &*fb_ptr };
    Some((fb.address as usize, fb.width as usize, fb.height as usize, fb.pitch as usize))
}

// --- Memory map ----------------------------------------------------------

#[repr(C)]
struct MemmapRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<MemmapResponse>,
}

// MAKE THIS PUBLIC so memory.rs can read `entry_count` and `entries`
#[repr(C)]
pub struct MemmapResponse {
    pub revision: u64,
    pub entry_count: u64,
    pub entries: *const *const MemmapEntry,
}

// MAKE THIS PUBLIC so memory.rs can read `base`, `length`, and `typ`
#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub typ: u64,
}

// MAKE THIS PUBLIC if memory.rs needs to filter by usable memory
pub const MEMMAP_USABLE: u64 = 0;
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5; // Add this if memory.rs needs to reclaim bootloader memory

#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest {
    id: [COMMON_MAGIC_0, COMMON_MAGIC_1, 0x67cf3d9d378a806f, 0xe304acdfc50c3c62],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// Returns the raw memory map response from Limine so the physical memory manager can parse it.
pub fn memmap() -> Option<&'static MemmapResponse> {
    let resp = MEMMAP_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    Some(unsafe { &*resp })
}

/// (entry count, total usable bytes) -- a summary, not the full map.
pub fn memmap_summary() -> Option<(usize, u64)> {
    let resp = MEMMAP_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    let resp = unsafe { &*resp };
    if resp.entries.is_null() {
        return Some((0, 0));
    }
    let mut usable = 0u64;
    for i in 0..resp.entry_count {
        let entry_ptr = unsafe { *resp.entries.add(i as usize) };
        if entry_ptr.is_null() {
            continue;
        }
        let entry = unsafe { &*entry_ptr };
        if entry.typ == MEMMAP_USABLE {
            usable += entry.length;
        }
    }
    Some((resp.entry_count as usize, usable))
}


// --- Module (used for the ramdisk; see limine.conf's module_path) -------

#[repr(C)]
struct ModuleRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<ModuleResponse>,
}

#[repr(C)]
struct ModuleResponse {
    revision: u64,
    module_count: u64,
    modules: *const *const LimineFile,
}

#[repr(C)]
struct LimineUuid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}

#[repr(C)]
struct LimineFile {
    revision: u64,
    address: *mut u8,
    size: u64,
    path: *const u8,
    string: *const u8,
    media_type: u32,
    unused: u32,
    tftp_ipv4: [u8; 4],
    tftp_port: u32,
    partition_index: u32,
    mbr_disk_id: u32,
    gpt_disk_uuid: LimineUuid,
    gpt_part_uuid: LimineUuid,
    part_uuid: LimineUuid,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static MODULE_REQUEST: ModuleRequest = ModuleRequest {
    id: [COMMON_MAGIC_0, COMMON_MAGIC_1, 0x3e7e279702be32af, 0xca1c4f3bd1280cee],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// (address, size) of the first module Limine loaded (the ramdisk
/// configured via `module_path` in limine.conf). `address` is already
/// a usable (HHDM) virtual pointer.
pub fn first_module() -> Option<(usize, usize)> {
    let resp = MODULE_REQUEST.response.load(Ordering::SeqCst);
    if resp.is_null() {
        return None;
    }
    let resp = unsafe { &*resp };
    if resp.module_count == 0 || resp.modules.is_null() {
        return None;
    }
    let file_ptr = unsafe { *resp.modules };
    if file_ptr.is_null() {
        return None;
    }
    let file = unsafe { &*file_ptr };
    Some((file.address as usize, file.size as usize))
}




// --- RSDP (ACPI) ---------------------------------------------------------

#[repr(C)]
struct RsdpRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicPtr<RsdpResponse>,
}

#[repr(C)]
struct RsdpResponse {
    revision: u64,
    address: *mut u8,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static RSDP_REQUEST: RsdpRequest = RsdpRequest {
    id: [
        COMMON_MAGIC_0,
        COMMON_MAGIC_1,
        0xc5e77b6b397e7b43,
        0x27637845accdcf3c,
    ],
    revision: 0,
    response: AtomicPtr::new(core::ptr::null_mut()),
};

/// Returns the ACPI RSDP address exactly as Limine reported it --
/// physical, per PROTOCOL.md, at exactly base revision 3 (what
/// mitosOS requests -- see BASE_REVISION above; every other revision,
/// old or 4+, gets it HHDM-virtual instead).
///
/// Deliberately *not* translated here anymore: an earlier version of
/// this function did the phys_to_virt translation internally and
/// returned a virtual pointer, on the (PROTOCOL.md-documented, and
/// confirmed correct by this address being raw/untranslated at the
/// wire) assumption that translated-physical was all that was needed
/// -- but that alone wasn't sufficient to make the RSDP parse
/// correctly in practice (still failed after translating), so
/// hal::acpi::init() now owns both the translation *and* a fallback
/// to the raw address if the translated one doesn't validate,
/// logging which one actually worked. Keeping this function a
/// faithful, untranslated passthrough of what Limine gave us is what
/// makes that fallback possible.
pub fn rsdp() -> Option<usize> {
    let resp = RSDP_REQUEST.response.load(Ordering::SeqCst);

    if resp.is_null() {
        return None;
    }

    let resp = unsafe { &*resp };

    if resp.address.is_null() {
        return None;
    }

    Some(resp.address as usize)
}


// --- Requests section markers --------------------------------------------
//
// Honoured (not just hinted) at base revision 2+, which we request --
// see linker_x86.ld for the sections these land in.
#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START_MARKER: [u64; 4] = [
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
];

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END_MARKER: [u64; 2] = [0xadc0e0531bb10d03, 0x9572709f31764c62];

pub fn detected() -> bool {
    HHDM_REQUEST.response.load(Ordering::SeqCst) != core::ptr::null_mut()
        || FRAMEBUFFER_REQUEST.response.load(Ordering::SeqCst) != core::ptr::null_mut()
        || MEMMAP_REQUEST.response.load(Ordering::SeqCst) != core::ptr::null_mut()
        || MODULE_REQUEST.response.load(Ordering::SeqCst) != core::ptr::null_mut()
        || EXECUTABLE_ADDRESS_REQUEST.response.load(Ordering::SeqCst) != core::ptr::null_mut()
}
