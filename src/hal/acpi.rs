//! ACPI (Advanced Configuration and Power Interface) Parser
//! 
//! Responsible for discovering hardware tables provided by the firmware.

use core::mem;

/// See `memory::phys_to_virt`'s doc comment.
#[inline]
fn phys_to_virt(phys: usize) -> usize {
    crate::memory::phys_to_virt(phys)
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
    let rsdp_virt = match get_limine_rsdp() {
        Ok(addr) => addr,

        Err(e) => {
            ulog!(
                "ACPI: {}",
                e
            );
            return;
        }
    };

    let root_table = match parse_rsdp(rsdp_virt) {
        Ok(root) => root,

        Err(e) => {
            ulog!(
                "ACPI: Failed to parse RSDP: {}",
                e
            );
            return;
        }
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

