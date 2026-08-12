//! ACPI (Advanced Configuration and Power Interface) Parser
//! 
//! Responsible for discovering hardware tables provided by the firmware.

use core::mem;
use limine::request::AcpiRequest;

/// The Limine ACPI request. The bootloader will fill this with the RSDP address.
#[used]
#[link_section = ".requests"]
static ACPI_REQUEST: AcpiRequest = AcpiRequest::new();

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
    if let Some(response) = ACPI_REQUEST.get_response() {
        // The `rsdp()` method returns a pointer. We cast it to usize for easy handling.
        let rsdp_addr = response.rsdp() as *const _ as usize;
        
        if rsdp_addr == 0 {
            return Err("Limine provided a null RSDP address");
        }
        Ok(rsdp_addr)
    } else {
        Err("Limine ACPI request did not receive a response")
    }
}

/// Parses the ACPI root pointer to find the main table array.
/// Returns the physical address of the RSDT (32-bit) or XSDT (64-bit).
pub fn parse_rsdp(rsdp_virtual_addr: usize) -> Result<usize, &'static str> {
    unsafe {
        // We do NOT use phys_to_virt here because Limine already provides 
        // a mapped, directly usable higher-half virtual address.
        let rsdp = &*(rsdp_virtual_addr as *const RsdpDescriptor);
        
        if &rsdp.signature != b"RSD PTR " {
            return Err("Invalid RSDP signature");
        }

        // Check if ACPI 2.0+ (revision 2 or higher)
        if rsdp.revision >= 2 {
            let rsdp20 = &*(rsdp_virtual_addr as *const RsdpDescriptor20);
            if !rsdp20.is_valid_extended() {
                return Err("Invalid ACPI 2.0 extended checksum");
            }
            // Note: The XSDT address inside the table is usually a physical address.
            // You will need to map this or use phys_to_virt when parsing the XSDT later.
            Ok(rsdp20.xsdt_address as usize)
        } else {
            // ACPI 1.0 fallback
            if !rsdp.is_valid() {
                return Err("Invalid ACPI 1.0 checksum");
            }
            // Note: The RSDT address is a physical address.
            Ok(rsdp.rsdt_address as usize)
        }
    }
}
