//! I/O APIC driver: discovery + redirection-table programming.
//!
//! Only discovery (init_from_madt, below -- reads IOAPICVER and logs
//! it) runs automatically at boot right now. Actually reprogramming
//! any redirection entry (moving a legacy IRQ off the 8259 PICs onto
//! the IO-APIC) is deliberately left as a follow-up rather than wired
//! in here: interrupts.rs's remap_pic() is still the active, working
//! delivery path for the timer and UART IRQs, and touching those same
//! GSIs' redirection entries without first masking the PICs would
//! risk double delivery. set_redirection_entry below is a complete,
//! ready-to-use building block for that migration once it's wanted --
//! see hal::smp's module doc comment for the fuller "next subsystem"
//! framing.

use crate::memory::phys_to_virt;

macro_rules! ulog {
    ($($arg:tt)*) => {{
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_fmt(
            &mut uart,
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}

const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;
const IOAPICVER: u32 = 0x01;

fn read_reg(mmio_base: usize, reg: u32) -> u32 {
    unsafe {
        let sel = phys_to_virt(mmio_base + IOREGSEL) as *mut u32;
        let win = phys_to_virt(mmio_base + IOWIN) as *const u32;
        core::ptr::write_volatile(sel, reg);
        core::ptr::read_volatile(win)
    }
}

#[allow(dead_code)]
fn write_reg(mmio_base: usize, reg: u32, value: u32) {
    unsafe {
        let sel = phys_to_virt(mmio_base + IOREGSEL) as *mut u32;
        let win = phys_to_virt(mmio_base + IOWIN) as *mut u32;
        core::ptr::write_volatile(sel, reg);
        core::ptr::write_volatile(win, value);
    }
}

/// A redirection table entry, decoded from the pair of 32-bit
/// registers (`0x10 + gsi_index*2`, `0x11 + gsi_index*2`) each GSI
/// occupies.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct RedirectionEntry {
    pub vector: u8,
    /// 000=Fixed, 001=Lowest Priority, 010=SMI, 100=NMI, 111=ExtINT.
    pub delivery_mode: u8,
    pub logical_dest: bool,
    pub polarity_low: bool,
    pub trigger_level: bool,
    pub masked: bool,
    pub dest: u8,
}

impl RedirectionEntry {
    fn to_bits(self) -> u64 {
        let mut low: u32 = self.vector as u32;
        low |= (self.delivery_mode as u32 & 0x7) << 8;
        if self.logical_dest {
            low |= 1 << 11;
        }
        if self.polarity_low {
            low |= 1 << 13;
        }
        if self.trigger_level {
            low |= 1 << 15;
        }
        if self.masked {
            low |= 1 << 16;
        }
        let high: u32 = (self.dest as u32) << 24;
        ((high as u64) << 32) | low as u64
    }
}

/// Read-only discovery: logs each MADT-reported IO-APIC's version and
/// redirection-entry count. Writes nothing -- see this module's doc
/// comment for why.
pub fn init_from_madt() {
    let madt = crate::hal::madt::MADT.lock();
    if madt.ioapic_count == 0 {
        ulog!("IOAPIC: none reported by MADT");
        return;
    }
    for ioapic in &madt.ioapics[..madt.ioapic_count] {
        let ver = read_reg(ioapic.address as usize, IOAPICVER);
        let version = ver & 0xFF;
        let max_entries = ((ver >> 16) & 0xFF) + 1;
        ulog!(
            "IOAPIC: id={} @ 0x{:X}, GSI base {}, version 0x{:X}, {} redirection entries",
            ioapic.id,
            ioapic.address,
            ioapic.gsi_base,
            version,
            max_entries
        );
    }
}

/// Programs GSI `gsi` on whichever discovered IO-APIC covers it (per
/// MADT's gsi_base ranges) with `entry`. Not called anywhere yet at
/// boot -- see this module's doc comment.
#[allow(dead_code)]
pub fn set_redirection_entry(gsi: u32, entry: RedirectionEntry) -> Result<(), &'static str> {
    let madt = crate::hal::madt::MADT.lock();
    for ioapic in &madt.ioapics[..madt.ioapic_count] {
        let ver = read_reg(ioapic.address as usize, IOAPICVER);
        let max_entries = ((ver >> 16) & 0xFF) + 1;
        if gsi >= ioapic.gsi_base && gsi < ioapic.gsi_base + max_entries {
            let index = gsi - ioapic.gsi_base;
            let bits = entry.to_bits();
            write_reg(ioapic.address as usize, 0x10 + index * 2, (bits & 0xFFFF_FFFF) as u32);
            write_reg(ioapic.address as usize, 0x11 + index * 2, (bits >> 32) as u32);
            return Ok(());
        }
    }
    Err("GSI not covered by any discovered IO-APIC")
}
