//! PCI Bus Enumerator and AHCI HAL Interface for mitosOS.
#![cfg(target_arch = "x86_64")]

use alloc::vec::Vec;
use crate::drivers::ahci::{AhciController, DeviceKind};
use crate::addr::{PhysAddr, VirtAddr};
use core::fmt::Write;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

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

pub fn read_config_32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let address = 1u32 << 31
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);

    unsafe {
        outl(CONFIG_ADDRESS, address);
        inl(CONFIG_DATA)
    }
}

pub fn read_config_16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let value = read_config_32(bus, slot, func, offset);
    ((value >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub fn read_config_8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let value = read_config_32(bus, slot, func, offset);
    ((value >> ((offset & 3) * 8)) & 0xFF) as u8
}

pub struct KernelHal {
    pub phys_mem_offset: u64,
}

impl crate::drivers::ahci::Hal for KernelHal {
    unsafe fn map_mmio(&mut self, phys: PhysAddr, size: usize) -> VirtAddr {
        let phys_addr = phys.as_u64() as usize;
        let page_start = phys_addr & !0xFFF;
        let page_end = (phys_addr + size.max(1) + 0xFFF) & !0xFFF;

        let current_root: usize;
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) current_root, options(nomem, nostack));
        }
        let root = (current_root & !0xFFF) as *mut crate::vmm::arch::PageTable;

        let flags = crate::memory::MapFlags {
            writable: true,
            user_accessible: false,
            execute_disable: true,
            device: true,
        };

       let mut page = page_start;
          unsafe {while page < page_end {
            let virt_page = page + (self.phys_mem_offset as usize);
            let _ = crate::vmm::arch::map_page(root, virt_page, page, flags);
            page += 0x1000;}}

        VirtAddr::new((phys_addr as u64) + self.phys_mem_offset)
    }

    unsafe fn alloc_dma(&mut self, size: usize) -> Option<(PhysAddr, VirtAddr)> {
        let frames_needed = (size + 4095) / 4096;
        let mut pmm = crate::memory::PHYSICAL_PMM.lock();
        let first_frame_idx = pmm.allocate_next_frame()?;
        let start_phys = PhysAddr::new((first_frame_idx * 4096) as u64);

        let virt = VirtAddr::new(start_phys.as_u64() + self.phys_mem_offset);
        unsafe {
            core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, frames_needed * 4096);
        }
        Some((start_phys, virt))
    }

    unsafe fn virt_to_phys(&self, virt: VirtAddr) -> Option<PhysAddr> {
        Some(PhysAddr::new(virt.as_u64() - self.phys_mem_offset))
    }

    fn wait_micros(&self, micros: u32) {
        for _ in 0..(micros as u64 * 1000) {
            core::hint::spin_loop();
        }
    }
}

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
    pub bar5: u32,
}

pub fn init_ahci_devices(uart: &mut impl Write) {
    for dev in scan_buses() {
        if dev.class == 0x01 && dev.subclass == 0x06 {
            let abar_phys = PhysAddr::new(dev.bar5 as u64);
            let mut hal = KernelHal { phys_mem_offset: crate::hal::acpi::PHYS_MEM_OFFSET as u64 };

            match unsafe { AhciController::new(abar_phys, &mut hal) } {
                Ok(mut ahci_controller) => {
                    let _ = writeln!(uart, "AHCI Controller initialized successfully!");
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
                }
                Err(e) => {
                    let _ = writeln!(uart, "Failed to initialize AHCI controller: {:?}", e);
                }
            }
        }
    }
}

pub fn scan_buses() -> Vec<PciDevice> {
    let mut devices = Vec::new();
    for bus in 0..=255 {
        for slot in 0..32 {
            let vendor_id = read_config_16(bus, slot, 0, 0x00);
            if vendor_id == 0xFFFF { continue; }

            let device_id = read_config_16(bus, slot, 0, 0x02);
            let class = read_config_8(bus, slot, 0, 0x0B);
            let subclass = read_config_8(bus, slot, 0, 0x0A);
            let prog_if = read_config_8(bus, slot, 0, 0x09);
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
