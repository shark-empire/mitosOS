//! ACPI (Advanced Configuration and Power Interface) Parser
//! 
//! Responsible for discovering hardware tables provided by the firmware.

use core::mem;

/// See `memory::phys_to_virt`'s doc comment.
#[inline]
fn phys_to_virt(phys: usize) -> usize {
    crate::memory::phys_to_virt(phys)
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
            crate::println!(
                "ACPI: {}",
                e
            );
            return;
        }
    };

    let root_table = match parse_rsdp(rsdp_virt) {
        Ok(root) => root,

        Err(e) => {
            crate::println!(
                "ACPI: Failed to parse RSDP: {}",
                e
            );
            return;
        }
    };

    match root_table {
        RootTable::Xsdt(addr) => {
            crate::println!(
                "ACPI: Using XSDT (ACPI 2.0+)"
            );

            crate::println!(
                "ACPI: XSDT at 0x{:X}",
                addr
            );

            parse_xsdt(addr);
        }

        RootTable::Rsdt(addr) => {
            crate::println!(
                "ACPI: Using RSDT (ACPI 1.0/legacy)"
            );

            crate::println!(
                "ACPI: RSDT at 0x{:X}",
                addr
            );

            parse_rsdt(addr);
        }
    }
}
fn parse_xsdt(xsdt_addr: usize) {
    unsafe {
        let xsdt_virt = xsdt_addr as *const SdtHeader;
        let xsdt = &*xsdt_virt;

        let signature =
            core::ptr::addr_of!(xsdt.signature).read_unaligned();

        if signature != *b"XSDT" {
            crate::println!("ACPI: Invalid XSDT signature");
            return;
        }

        if !xsdt.is_valid() {
            crate::println!("ACPI: Invalid XSDT checksum");
            return;
        }

        let xsdt_length =
            core::ptr::addr_of!(xsdt.length)
                .read_unaligned() as usize;

        if xsdt_length < mem::size_of::<SdtHeader>() {
            crate::println!("ACPI: Invalid XSDT length");
            return;
        }

        let entries_count =
            (xsdt_length - mem::size_of::<SdtHeader>()) / 8;

        let entries_ptr =
            (xsdt_addr + mem::size_of::<SdtHeader>()) as *const u64;

        crate::println!(
            "ACPI: Parsing {} XSDT entries...",
            entries_count
        );

        for i in 0..entries_count {
            let table_addr =
                core::ptr::addr_of!(*entries_ptr.add(i))
                    .read_unaligned() as usize;

            parse_acpi_table(table_addr);
        }
    }
}

fn parse_rsdt(rsdt_addr: usize) {
    unsafe {
        let rsdt_virt = rsdt_addr as *const SdtHeader;
        let rsdt = &*rsdt_virt;

        let signature =
            core::ptr::addr_of!(rsdt.signature).read_unaligned();

        if signature != *b"RSDT" {
            crate::println!("ACPI: Invalid RSDT signature");
            return;
        }

        if !rsdt.is_valid() {
            crate::println!("ACPI: Invalid RSDT checksum");
            return;
        }

        let rsdt_length =
            core::ptr::addr_of!(rsdt.length)
                .read_unaligned() as usize;

        if rsdt_length < mem::size_of::<SdtHeader>() {
            crate::println!("ACPI: Invalid RSDT length");
            return;
        }

        let entries_count =
            (rsdt_length - mem::size_of::<SdtHeader>()) / 4;

        let entries_ptr =
            (rsdt_addr + mem::size_of::<SdtHeader>()) as *const u32;

        crate::println!(
            "ACPI: Parsing {} RSDT entries...",
            entries_count
        );

        for i in 0..entries_count {
            let table_addr =
                core::ptr::addr_of!(*entries_ptr.add(i))
                    .read_unaligned() as usize;

            parse_acpi_table(table_addr);
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
            crate::println!(
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

            crate::println!(
                "ACPI: Found MADT (Local APIC at 0x{:X})",
                local_apic_address
            );
        } else if signature == *b"MCFG" {
            crate::println!(
                "ACPI: Found MCFG (PCIe Configuration Space)"
            );
        } else if signature == *b"FACP" {
            crate::println!(
                "ACPI: Found FADT"
            );
        } else if signature == *b"HPET" {
            crate::println!(
                "ACPI: Found HPET"
            );
        } else {
            crate::println!(
                "ACPI: Found table {:?}",
                signature
            );
        }
    }
}

