//! ACPI (Advanced Configuration and Power Interface) Parser
//! 
//! Responsible for discovering hardware tables provided by the firmware.

use core::mem;

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

/// Parses the ACPI root pointer to find the main table array.
/// Returns the physical address of the RSDT (32-bit) or XSDT (64-bit).
pub fn parse_rsdp(rsdp_phys_addr: usize) -> Result<usize, &'static str> {
    unsafe {
        let rsdp = &*(rsdp_phys_addr as *const RsdpDescriptor);
        
        if &rsdp.signature != b"RSD PTR " {
            return Err("Invalid RSDP signature");
        }

        // Check if ACPI 2.0+ (revision 2 or higher)
        if rsdp.revision >= 2 {
            let rsdp20 = &*(rsdp_phys_addr as *const RsdpDescriptor20);
            if !rsdp20.is_valid_extended() {
                return Err("Invalid ACPI 2.0 extended checksum");
            }
            Ok(rsdp20.xsdt_address as usize)
        } else {
            // ACPI 1.0 fallback
            if !rsdp.is_valid() {
                return Err("Invalid ACPI 1.0 checksum");
            }
            Ok(rsdp.rsdt_address as usize)
        }
    }
}

// src/hal/acpi.rs

/// Scans the legacy BIOS memory region (0x000E0000 - 0x000FFFFF) to locate the RSDP.
pub fn find_rsdp_legacy() -> Option<usize> {
    let start_addr: usize = 0x000E_0000;
    let end_addr: usize = 0x000F_FFFF;

    // The RSDP is guaranteed to be on a 16-byte boundary
    let mut current_addr = start_addr;
    
    while current_addr < end_addr {
        // Read 8 bytes safely to check the signature
        let signature = unsafe { core::slice::from_raw_parts(current_addr as *const u8, 8) };
        
        if signature == b"RSD PTR " {
            // We found the signature, now validate the checksum
            let rsdp = unsafe { &*(current_addr as *const RsdpDescriptor) };
            
            // Check if it's an ACPI 2.0 extended descriptor first
            if rsdp.revision >= 2 {
                let rsdp20 = unsafe { &*(current_addr as *const RsdpDescriptor20) };
                if rsdp20.is_valid_extended() {
                    return Some(current_addr);
                }
            }
            
            // Fallback to ACPI 1.0 validation
            if rsdp.is_valid() {
                return Some(current_addr);
            }
        }
        
        current_addr += 16;
    }

    None
}

