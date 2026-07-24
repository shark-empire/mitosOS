//! PCI (Peripheral Component Interconnect) Bus Enumerator

use alloc::vec::Vec;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// --- Low-Level 32-bit Port I/O ---

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

// --- PCI Configuration Readers ---

/// Reads a 32-bit word from the PCI configuration space.
pub fn read_config_32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let address = 1u32 << 31                     // Enable Bit
        | ((bus as u32) << 16)                   // Bus Number
        | ((slot as u32) << 11)                  // Device/Slot Number
        | ((func as u32) << 8)                   // Function Number
        | ((offset as u32) & 0xFC);              // Register Offset (must be word-aligned)

    unsafe {
        outl(CONFIG_ADDRESS, address);
        inl(CONFIG_DATA)
    }
}

pub fn read_config_16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let value = read_config_32(bus, slot, func, offset);
    // Shift down to get the exact 16-bit chunk we requested
    ((value >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub fn read_config_8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let value = read_config_32(bus, slot, func, offset);
    // Shift down to get the exact 8-bit chunk we requested
    ((value >> ((offset & 3) * 8)) & 0xFF) as u8
}

// --- Device Representation ---

#[derive(Debug)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    /// Base Address Register 5 (Crucial for AHCI MMIO)
    pub bar5: u32,
}

// --- Enumerator ---

/// Scans the PCI buses and returns a list of all attached devices.
pub fn scan_buses() -> Vec<PciDevice> {
    let mut devices = Vec::new();

    // The PCI standard supports up to 256 buses, with 32 slots per bus.
    for bus in 0..=255 {
        for slot in 0..32 {
            // Offset 0x00 contains the Vendor ID. 
            // If it returns 0xFFFF, the slot is physically empty.
            let vendor_id = read_config_16(bus, slot, 0, 0x00);
            
            if vendor_id == 0xFFFF {
                continue; 
            }

            // Offset 0x02 is Device ID
            let device_id = read_config_16(bus, slot, 0, 0x02);
            
            // Offsets for Class, Subclass, and ProgIF
            let class = read_config_8(bus, slot, 0, 0x0B);
            let subclass = read_config_8(bus, slot, 0, 0x0A);
            let prog_if = read_config_8(bus, slot, 0, 0x09);
            
            // Offset 0x24 is BAR5
            let bar5 = read_config_32(bus, slot, 0, 0x24);

            devices.push(PciDevice {
                bus, slot, func: 0,
                vendor_id, device_id,
                class, subclass, prog_if,
                bar5,
            });
        }
    }

    devices
}
