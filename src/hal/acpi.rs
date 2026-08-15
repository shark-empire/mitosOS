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
    /// Validates the RSDP checksum.
    /// The sum of all bytes in the structure must equal 0 (modulo 256).
    pub fn is_valid(&self) -> bool {
        let size = mem::size_of::<Self>();
        let ptr = self as *const _ as *const u8;
        let mut sum: u8 = 0;
        
        unsafe {
            for i in 0..size {
                sum = sum.wrapping_add(*ptr.add(i));
            }
        }
        
        sum == 0 && &self.signature == b"RSD PTR "
    }
}

impl RsdpDescriptor20 {
    /// Validates the extended ACPI 2.0 checksum.
    pub fn is_valid_extended(&self) -> bool {
        let size = mem::size_of::<Self>();
        let ptr = self as *const _ as *const u8;
        let mut sum: u8 = 0;
        
        unsafe {
            for i in 0..size {
                sum = sum.wrapping_add(*ptr.add(i));
            }
        }
        
        sum == 0 && self.first_part.is_valid()
    }
}

/// Fetches the RSDP address directly from the Limine bootloader response.
pub fn get_limine_rsdp() -> Result<usize, &'static str> {
    crate::limine::rsdp().ok_or("Limine did not provide an RSDP address")
}

/// Parses the ACPI root pointer to find the main table array.
/// Returns the physical address of the RSDT (32-bit) or XSDT (64-bit).
pub fn parse_rsdp(rsdp_virtual_addr: usize) -> Result<usize, &'static str> {
    unsafe {
        let rsdp = &*(rsdp_virtual_addr as *const RsdpDescriptor);

        if &rsdp.signature != b"RSD PTR " {
            return Err("Invalid RSDP signature");
        }

        let revision = core::ptr::addr_of!(rsdp.revision)
            .read_unaligned();

        if revision >= 2 {
            let rsdp20 = &*(rsdp_virtual_addr as *const RsdpDescriptor20);

            if !rsdp20.is_valid_extended() {
                return Err("Invalid ACPI 2.0 extended checksum");
            }

            let xsdt_address = core::ptr::addr_of!(rsdp20.xsdt_address)
                .read_unaligned();

            Ok(xsdt_address as usize)
        } else {
            if !rsdp.is_valid() {
                return Err("Invalid ACPI 1.0 checksum");
            }

            let rsdt_address = core::ptr::addr_of!(rsdp.rsdt_address)
                .read_unaligned();

            Ok(rsdt_address as usize)
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
        let size = unsafe {
    core::ptr::addr_of!(self.length).read_unaligned() as usize
            };
        let ptr = self as *const _ as *const u8;
        let mut sum: u8 = 0;
        unsafe {
            for i in 0..size {
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
    match get_limine_rsdp() {
        Ok(rsdp_virt) => {
            match parse_rsdp(rsdp_virt) {
                Ok(xsdt_phys) => {
                    // XSDT address from RSDP is physical; convert to virtual using HHDM
                    let xsdt_virt = phys_to_virt(xsdt_phys) as *const SdtHeader;
                    unsafe {
                        let xsdt = &*xsdt_virt;
                        if &xsdt.signature != b"XSDT" || !xsdt.is_valid() {
                            crate::println!("ACPI: Invalid XSDT signature or checksum");
                            return;
                        }

                        let xsdt_length = core::ptr::addr_of!(xsdt.length)
                       .read_unaligned() as usize;

                        let entries_count =
                       (xsdt_length - mem::size_of::<SdtHeader>()) / 8;
                        let entries_ptr = (xsdt_virt as usize + mem::size_of::<SdtHeader>()) as *const u64;

                        crate::println!("ACPI: Parsing {} system description tables...", entries_count);

                        for i in 0..entries_count {
                            let table_phys = *entries_ptr.add(i) as usize;
                            let table_virt = phys_to_virt(table_phys) as *const SdtHeader;
                            let header = &*table_virt;

                            if &header.signature == b"APIC" {
                          let madt = &*(table_virt as *const Madt);

                         let local_apic_address = core::ptr::addr_of!(madt.local_apic_address)
                        .read_unaligned();

                      crate::println!(
                     "ACPI: Found MADT (Local APIC at 0x{:X})",
                         local_apic_address
                           );
                  } else if &header.signature == b"MCFG" {
               crate::println!("ACPI: Found MCFG (PCIe Configuration Space)");
                         }
                        }
                    }
                }
                Err(e) => crate::println!("ACPI: Failed to parse RSDP: {e}"),
            }
        }
        Err(e) => crate::println!("ACPI: {e}"),
    }
}

