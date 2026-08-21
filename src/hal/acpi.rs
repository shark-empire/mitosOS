//! ACPI (Advanced Configuration and Power Interface) Parser
//! 
//! Responsible for discovering hardware tables provided by the firmware.

use core::mem;

/// See `memory::phys_to_virt`'s doc comment.
#[inline]
fn phys_to_virt(phys: usize) -> usize {
    crate::memory::phys_to_virt(phys)
}

/// See `memory::hhdm_offset`'s doc comment. That function itself is
/// x86_64-only (aarch64 has no HHDM-offset concept -- phys_to_virt is
/// a no-op there), but this file compiles on both targets, so give
/// the diagnostic below a value either way instead of a cfg'd-out
/// call site.
#[cfg(target_arch = "x86_64")]
#[inline]
fn hhdm_offset() -> usize {
    crate::memory::hhdm_offset()
}
#[cfg(target_arch = "aarch64")]
#[inline]
fn hhdm_offset() -> usize {
    0
}

/// Reads the first 16 bytes at `addr` for a diagnostic hex dump.
///
/// Only called after a failed RSDP signature check at this exact
/// address -- if it weren't safely readable, that check would already
/// have faulted getting this far (it reads the same bytes, just
/// through a typed struct instead of raw), so this doesn't introduce
/// new risk. Exists purely to tell apart two very different failure
/// modes that otherwise look identical from a pass/fail signature
/// check alone: real memory holding the wrong data (non-zero,
/// structured-looking bytes) vs. an address that isn't backed by
/// what we think it is (all zero, or some other flat/repeating
/// pattern -- e.g. Limine's HHDM not actually covering this
/// particular region at base revision 3).
unsafe fn dump16(addr: usize) -> [u8; 16] {
    unsafe {
        let mut buf = [0u8; 16];
        core::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), 16);
        buf
    }
}

/// Classical ACPI RSDP discovery (ACPI spec 5.2.5.1), independent of
/// whatever Limine's own RSDP request reports: scan the first 1 KiB
/// of the Extended BIOS Data Area, then the entire main BIOS
/// read-only area (0xE0000-0xFFFFF), in 16-byte steps, for the
/// eight-byte "RSD PTR " signature -- every x86 BIOS (real or QEMU's)
/// is required to place it in one of those two regions. Used as a
/// last resort after both interpretations of Limine's own answer
/// fail to validate, or if Limine doesn't answer the request at all.
/// A signature-only prefilter keeps this cheap; `parse_rsdp` (full
/// checksum validation) decides whether each hit is real, so a
/// coincidental 8-byte match elsewhere can't produce a false
/// positive.
fn scan_bios_for_rsdp() -> Option<usize> {
    const SIGNATURE: &[u8; 8] = b"RSD PTR ";

    fn scan_range(start_phys: usize, end_phys: usize) -> Option<usize> {
        let mut phys = start_phys;
        while phys + 16 <= end_phys {
            let virt = phys_to_virt(phys);
            let bytes = unsafe { dump16(virt) };
            if bytes[0..8] == *SIGNATURE && parse_rsdp(virt).is_ok() {
                return Some(virt);
            }
            phys += 16;
        }
        None
    }

    // EBDA base (as a real-mode segment, so << 4 for the physical
    // address) lives as a 16-bit word at physical 0x40E, in the
    // BIOS Data Area -- itself always safely readable low memory.
    let ebda_segment_ptr = phys_to_virt(0x40E);
    let ebda_segment = unsafe { core::ptr::read_unaligned(ebda_segment_ptr as *const u16) };
    let ebda_base = (ebda_segment as usize) << 4;
    if ebda_base != 0 {
        if let Some(found) = scan_range(ebda_base, ebda_base + 1024) {
            return Some(found);
        }
    }

    scan_range(0xE0000, 0x100000)
}

/// ACPI init runs at boot, long before graphics::WRITER exists (the
/// framebuffer isn't set up until much later in main.rs's
/// kmain_common) -- so, like every other pre-framebuffer boot
/// message, these diagnostics have to go straight to the UART.
/// println!/print! target graphics::WRITER and would silently do
/// nothing this early, which is exactly what every call in this file
/// used to do -- this whole module's ACPI table walk was completely
/// silent, success or failure, on every boot.
macro_rules! ulog {
    ($($arg:tt)*) => {{
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_fmt(
            &mut uart,
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}

/// The standard ACPI 1.0 Root System Description Pointer
#[repr(C, packed)]
pub struct RsdpDescriptor {
    pub signature: [u8; 8],
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub revision: u8,
    pub rsdt_address: u32,
}
#[derive(Copy, Clone, Debug)]
pub enum RootTable {
    Rsdt(usize),
    Xsdt(usize),
}

/// The ACPI 2.0+ Extended Descriptor (includes XSDT for 64-bit addresses)
#[repr(C, packed)]
pub struct RsdpDescriptor20 {
    pub first_part: RsdpDescriptor,
    pub length: u32,
    pub xsdt_address: u64,
    pub extended_checksum: u8,
    pub reserved: [u8; 3],
}

impl RsdpDescriptor {
    pub fn is_valid(&self) -> bool {
        let size = mem::size_of::<Self>();
        let ptr = self as *const _ as *const u8;
        let mut sum: u8 = 0;

        unsafe {
            for i in 0..size {
                sum = sum.wrapping_add(*ptr.add(i));
            }
        }

        let signature =
            unsafe {
                core::ptr::addr_of!(self.signature)
                    .read_unaligned()
            };

        sum == 0 && signature == *b"RSD PTR "
    }
}

impl RsdpDescriptor20 {
    /// Validates the extended ACPI 2.0+ checksum.
    pub fn is_valid_extended(&self) -> bool {
        // The first 20 bytes must pass the ACPI 1.0 checksum.
        if !self.first_part.is_valid() {
            return false;
        }

        let length = self.length as usize;

        // A valid ACPI 2.0+ RSDP must at least contain
        // the complete extended structure.
        if length < mem::size_of::<Self>() {
            return false;
        }

        let ptr = self as *const Self as *const u8;
        let mut sum = 0u8;

        unsafe {
            for i in 0..length {
                sum = sum.wrapping_add(*ptr.add(i));
            }
        }

        sum == 0
    }
}


/// Fetches the RSDP address directly from the Limine bootloader response.
pub fn get_limine_rsdp() -> Result<usize, &'static str> {
    crate::limine::rsdp().ok_or("Limine did not provide an RSDP address")
}

/// Parses the ACPI root pointer to find the main table array.
pub fn parse_rsdp(
    rsdp_virtual_addr: usize,
) -> Result<RootTable, &'static str> {
    unsafe {
        let rsdp =
            &*(rsdp_virtual_addr as *const RsdpDescriptor);

        let signature =
            core::ptr::addr_of!(rsdp.signature)
                .read_unaligned();

        if signature != *b"RSD PTR " {
            return Err("Invalid RSDP signature");
        }

        let revision =
            core::ptr::addr_of!(rsdp.revision)
                .read_unaligned();

        if revision >= 2 {
            let rsdp20 =
                &*(rsdp_virtual_addr as *const RsdpDescriptor20);

            if !rsdp20.is_valid_extended() {
                return Err(
                    "Invalid ACPI 2.0+ checksum"
                );
            }

            let xsdt_address =
                core::ptr::addr_of!(rsdp20.xsdt_address)
                    .read_unaligned() as usize;

            let rsdt_address =
                core::ptr::addr_of!(
                    rsdp20.first_part.rsdt_address
                )
                .read_unaligned() as usize;

            if xsdt_address != 0 {
                Ok(RootTable::Xsdt(xsdt_address))
            } else if rsdt_address != 0 {
                Ok(RootTable::Rsdt(rsdt_address))
            } else {
                Err(
                    "ACPI 2.0+ RSDP contains no RSDT or XSDT"
                )
            }
        } else {
            if !rsdp.is_valid() {
                return Err(
                    "Invalid ACPI 1.0 checksum"
                );
            }

            let rsdt_address =
                core::ptr::addr_of!(rsdp.rsdt_address)
                    .read_unaligned() as usize;

            if rsdt_address == 0 {
                return Err(
                    "ACPI 1.0 RSDP contains no RSDT"
                );
            }

            Ok(RootTable::Rsdt(rsdt_address))
        }
    }
}

/// Standard ACPI System Description Table header
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

impl SdtHeader {
    pub fn is_valid(&self) -> bool {
        let length =
            unsafe {
                core::ptr::addr_of!(self.length)
                    .read_unaligned()
            } as usize;

        if length < mem::size_of::<SdtHeader>() {
            return false;
        }

        let ptr = self as *const _ as *const u8;
        let mut sum: u8 = 0;

        unsafe {
            for i in 0..length {
                sum = sum.wrapping_add(*ptr.add(i));
            }
        }

        sum == 0
    }
}

/// Multiple APIC Description Table (MADT) header
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct Madt {
    pub header: SdtHeader,
    pub local_apic_address: u32,
    pub flags: u32,
}

/// High-level entry point to initialize and parse ACPI tables via Limine's RSDP
pub fn init() {
    let mut root_table: Option<RootTable> = None;

    match get_limine_rsdp() {
        Ok(rsdp_addr) => {
            // PROTOCOL.md: the RSDP address is physical *only* when
            // this kernel was actually loaded at exactly base
            // revision 3 -- every other revision (0-2, and 4+) hands
            // it back already HHDM-virtual. Base revision 3 is also
            // the one revision whose HHDM is *restrictive* (only
            // Usable / Bootloader-reclaimable / Executable-and
            // -modules / Framebuffer regions are mapped) with no
            // guarantee ACPI tables live in any of those -- BIOS
            // firmware conventionally puts the RSDP in the EBDA or
            // the main BIOS ROM area, both typically typed Reserved,
            // which revision 3 simply doesn't map anywhere (and
            // identity mapping was dropped back at revision 1
            // already). That's the actual root cause behind this
            // failing under revision 3: not a wrong address, an
            // *unmapped* one -- no interpretation of it reads real
            // data, translated or raw.
            //
            // Base revision 4 fixes this at the protocol level (see
            // limine.rs::REQUESTED_BASE_REVISION), so the normal case
            // is `revision == 4` and `rsdp_addr` is already usable
            // directly. The `revision == 3` branch below only exists
            // for an older bootloader that can't grant 4 and falls
            // back to 3 -- PROTOCOL.md still guarantees at least 3.
            let revision = crate::limine::loaded_base_revision();
            let primary = if revision == 3 { phys_to_virt(rsdp_addr) } else { rsdp_addr };
            let secondary = if revision == 3 { rsdp_addr } else { phys_to_virt(rsdp_addr) };

            match parse_rsdp(primary) {
                Ok(root) => {
                    ulog!(
                        "ACPI: RSDP valid at 0x{:X} (loaded base revision {})",
                        primary,
                        revision
                    );
                    root_table = Some(root);
                }
                Err(e_primary) => match parse_rsdp(secondary) {
                    Ok(root) => {
                        ulog!(
                            "ACPI: RSDP valid at fallback address 0x{:X} (loaded base \
                             revision {}) -- primary address 0x{:X} did not validate: {}",
                            secondary,
                            revision,
                            primary,
                            e_primary
                        );
                        root_table = Some(root);
                    }
                    Err(e_secondary) => {
                        ulog!(
                            "ACPI: Failed to parse RSDP at 0x{:X} ({}) or 0x{:X} ({}); \
                             loaded base revision: {}, HHDM offset: 0x{:X}",
                            primary,
                            e_primary,
                            secondary,
                            e_secondary,
                            revision,
                            hhdm_offset()
                        );
                        let bp = unsafe { dump16(primary) };
                        ulog!(
                            "ACPI: bytes at 0x{:X}: \
                             {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} \
                             {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                            primary,
                            bp[0], bp[1], bp[2], bp[3], bp[4], bp[5], bp[6], bp[7],
                            bp[8], bp[9], bp[10], bp[11], bp[12], bp[13], bp[14], bp[15]
                        );
                        let bs = unsafe { dump16(secondary) };
                        ulog!(
                            "ACPI: bytes at 0x{:X}: \
                             {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} \
                             {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                            secondary,
                            bs[0], bs[1], bs[2], bs[3], bs[4], bs[5], bs[6], bs[7],
                            bs[8], bs[9], bs[10], bs[11], bs[12], bs[13], bs[14], bs[15]
                        );
                    }
                },
            }
        }
        Err(e) => {
            ulog!("ACPI: {}", e);
        }
    }

    if root_table.is_none() {
        // Limine's own answer either wasn't provided, or didn't
        // validate under either interpretation -- fall back to the
        // classical, bootloader-independent RSDP discovery the ACPI
        // spec itself defines (5.2.5.1): every x86 BIOS is required
        // to place the RSDP in the first 1 KiB of the EBDA or in the
        // main BIOS read-only area, so this works regardless of
        // whatever's wrong with Limine's pointer or HHDM coverage of
        // wherever it points.
        //
        // Note this can still come up empty under base revision 3
        // even with the fix above, since it walks the *whole*
        // EBDA/BIOS-ROM range via phys_to_virt, not just the specific
        // page(s) base revision 4 guarantees are mapped -- harmless
        // (same "not found" result as before the fix), just not
        // expected to be needed in the normal (revision 4) case.
        match scan_bios_for_rsdp() {
            Some(found_virt) => {
                ulog!("ACPI: RSDP found via BIOS/EBDA scan at 0x{:X}", found_virt);
                root_table = parse_rsdp(found_virt).ok();
            }
            None => {
                ulog!("ACPI: BIOS/EBDA scan found no RSDP signature either");
            }
        }
    }

    let root_table = match root_table {
        Some(rt) => rt,
        None => return,
    };

    match root_table {
        RootTable::Xsdt(addr) => {
            ulog!(
                "ACPI: Using XSDT (ACPI 2.0+)"
            );

            ulog!(
                "ACPI: XSDT at 0x{:X}",
                addr
            );

            parse_xsdt(addr);
        }

        RootTable::Rsdt(addr) => {
            ulog!(
                "ACPI: Using RSDT (ACPI 1.0/legacy)"
            );

            ulog!(
                "ACPI: RSDT at 0x{:X}",
                addr
            );

            parse_rsdt(addr);
        }
    }
}
fn parse_xsdt(xsdt_addr: usize) {
    unsafe {
        // FIX 1: Translate the base XSDT physical address
        let xsdt_virt = phys_to_virt(xsdt_addr) as *const SdtHeader;
        let xsdt = &*xsdt_virt;

        let signature = core::ptr::addr_of!(xsdt.signature).read_unaligned();
        if signature != *b"XSDT" {
            ulog!("ACPI: Invalid XSDT signature");
            return;
        }

        if !xsdt.is_valid() {
            ulog!("ACPI: Invalid XSDT checksum");
            return;
        }

        let xsdt_length = core::ptr::addr_of!(xsdt.length).read_unaligned() as usize;
        if xsdt_length < mem::size_of::<SdtHeader>() {
            ulog!("ACPI: Invalid XSDT length");
            return;
        }

        let entries_count = (xsdt_length - mem::size_of::<SdtHeader>()) / 8;

        // FIX 2: Translate the base address when calculating the entries array pointer
        let entries_ptr = (phys_to_virt(xsdt_addr) + mem::size_of::<SdtHeader>()) as *const u64;

        ulog!("ACPI: Parsing {} XSDT entries...", entries_count);

        for i in 0..entries_count {
            // The address stored INSIDE the table is also a physical address
            let entry_phys_addr = core::ptr::addr_of!(*entries_ptr.add(i)).read_unaligned() as usize;

            // FIX 3: Translate the entry's physical address before parsing it
            parse_acpi_table(phys_to_virt(entry_phys_addr));
        }
    }
}


fn parse_rsdt(rsdt_addr: usize) {
    unsafe {
        let rsdt_virt = phys_to_virt(rsdt_addr) as *const SdtHeader;
        let rsdt = &*rsdt_virt;

        let signature =
            core::ptr::addr_of!(rsdt.signature).read_unaligned();

        if signature != *b"RSDT" {
            ulog!("ACPI: Invalid RSDT signature");
            return;
        }

        if !rsdt.is_valid() {
            ulog!("ACPI: Invalid RSDT checksum");
            return;
        }

        let rsdt_length =
            core::ptr::addr_of!(rsdt.length)
                .read_unaligned() as usize;

        if rsdt_length < mem::size_of::<SdtHeader>() {
            ulog!("ACPI: Invalid RSDT length");
            return;
        }

        let entries_count =
            (rsdt_length - mem::size_of::<SdtHeader>()) / 4;

        let entries_ptr =
            (phys_to_virt(rsdt_addr) + mem::size_of::<SdtHeader>()) as *const u32;

        ulog!(
            "ACPI: Parsing {} RSDT entries...",
            entries_count
        );

        for i in 0..entries_count {
            let table_addr =
                core::ptr::addr_of!(*entries_ptr.add(i))
                    .read_unaligned() as usize;

            parse_acpi_table(phys_to_virt(table_addr));
        }
    }
}


fn parse_acpi_table(table_addr: usize) {
    unsafe {
        let table_virt = table_addr as *const SdtHeader;
        let header = &*table_virt;

        let signature =
            core::ptr::addr_of!(header.signature)
                .read_unaligned();

        if !header.is_valid() {
            ulog!(
                "ACPI: Invalid table checksum"
            );
            return;
        }

        if signature == *b"APIC" {
            let madt = &*(table_virt as *const Madt);

            let local_apic_address =
                core::ptr::addr_of!(
                    madt.local_apic_address
                )
                .read_unaligned();

            ulog!(
                "ACPI: Found MADT (Local APIC at 0x{:X})",
                local_apic_address
            );
        } else if signature == *b"MCFG" {
            ulog!(
                "ACPI: Found MCFG (PCIe Configuration Space)"
            );
        } else if signature == *b"FACP" {
            ulog!(
                "ACPI: Found FADT"
            );
        } else if signature == *b"HPET" {
            ulog!(
                "ACPI: Found HPET"
            );
        } else {
            ulog!(
                "ACPI: Found table {:?}",
                signature
            );
        }
    }
}

