//! PCI (Peripheral Component Interconnect) Bus Enumerator
#![cfg(target_arch = "x86_64")]

use alloc::vec::Vec;
use crate::drivers::ahci::{AhciController, DeviceKind};
use x86_64::PhysAddr;

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


// Inside your PCI device iteration loop:
for dev in pci_devices{
if dev.class == 0x01 && dev.subclass == 0x06 {
    let abar_phys = PhysAddr::new(dev.bar5 as u64);
    
    // Instantiate your HAL using your frame allocator and memory offset
    let mut hal = KernelHal {
        phys_mem_offset: 0xFFFF_8000_0000_0000, // Update to match your kernel's physical memory offset
        frame_allocator: &mut FRAME_ALLOCATOR,     // Reference to your active frame allocator
    };

    match unsafe { AhciController::new(abar_phys, &mut hal) } {
        Ok(mut ahci_controller) => {
            let _ = writeln!(uart, "AHCI Controller initialized successfully!");

            // Iterate over all active ports to find connected disks
            for port in ahci_controller.iter_ports() {
                if port.kind() == DeviceKind::Sata {
                    let _ = writeln!(
                        uart,
                        "Found SATA Drive on Port {}: {} sectors (LBA48: {})",
                        port.index(),
                        port.sector_count(),
                        port.supports_lba48()
                    );
                }
            }

            // Example: Read sector 0 from Port 0 (if present)
            if let Some(disk) = ahci_controller.port_mut(0) {
                let mut sector_buf = [0u8; 512];
                if let Ok(()) = disk.read_sectors(&mut hal, 0, &mut sector_buf) {
                    let _ = writeln!(uart, "Successfully read MBR / Sector 0 from disk!");
                    // Pass sector_buf to your partition/filesystem parser (e.g., FAT32 mount)
                }
            }
        }
        Err(e) => {
            let _ = writeln!(uart, "Failed to initialize AHCI controller: {:?}", e);
        }
    }
}
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
