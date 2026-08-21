//! MADT (Multiple APIC Description Table) entry parsing.
//!
//! hal::acpi's parse_acpi_table already validates the MADT's checksum
//! and reads its fixed header (local_apic_address, flags) -- this
//! module is the missing second half: walking the variable-length
//! entry list that follows that fixed header, where the actual
//! per-CPU Local APIC IDs and the IO-APIC's own MMIO address live.
//! hal::smp needs both to bring up APs at all (the Local APIC IDs are
//! the SIPI target) and to program interrupt routing later (the
//! IO-APIC address / GSI base).
//!
//! Each entry is a simple TLV: `entry_type: u8, entry_length: u8`,
//! followed by `entry_length - 2` bytes of type-specific payload --
//! unknown types are skipped using their own length, so a future ACPI
//! revision adding new entry types can't desync this walk.

macro_rules! ulog {
    ($($arg:tt)*) => {{
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_fmt(
            &mut uart,
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}

/// Compile-time capacity for every per-CPU/per-IOAPIC table in the
/// hal:: layer (this module, hal::smp, gdt.rs's per-AP GDT/TSS array).
/// A fixed array, not a Vec, so none of this bookkeeping depends on
/// the heap already being initialized. 64 covers every real MP system
/// and every QEMU `-smp` test configuration in practice; a system
/// reporting more just has the extras logged and dropped rather than
/// overflowing anything.
pub const MAX_CPUS: usize = 64;
pub const MAX_IOAPICS: usize = 8;
pub const MAX_ISOS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    ConformsToBus,
    ActiveHigh,
    ActiveLow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerMode {
    ConformsToBus,
    Edge,
    Level,
}

fn decode_mps_flags(flags: u16) -> (Polarity, TriggerMode) {
    let polarity = match flags & 0x3 {
        1 => Polarity::ActiveHigh,
        3 => Polarity::ActiveLow,
        _ => Polarity::ConformsToBus,
    };
    let trigger = match (flags >> 2) & 0x3 {
        1 => TriggerMode::Edge,
        3 => TriggerMode::Level,
        _ => TriggerMode::ConformsToBus,
    };
    (polarity, trigger)
}

/// A usable CPU (Processor Local APIC or Local x2APIC entry that was
/// either enabled, or not-yet-enabled-but-hotplug-capable -- see
/// push_cpu). `apic_id` is widened to u32 to also hold x2APIC IDs
/// (type-9 entries; xAPIC-only systems never report one above 255).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessorLocalApic {
    pub acpi_processor_id: u8,
    pub apic_id: u32,
    pub enabled: bool,
    pub online_capable: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct InterruptSourceOverride {
    pub bus: u8,
    pub source_irq: u8,
    pub gsi: u32,
    pub polarity: Polarity,
    pub trigger: TriggerMode,
}

pub struct MadtData {
    pub found: bool,
    /// Local APIC MMIO base -- from the MADT's fixed header, unless a
    /// type-5 Local APIC Address Override entry replaces it (that's
    /// the only entry allowed to widen this past 32 bits).
    pub local_apic_address: u64,
    pub legacy_pic_present: bool,
    pub cpus: [ProcessorLocalApic; MAX_CPUS],
    pub cpu_count: usize,
    pub ioapics: [IoApicInfo; MAX_IOAPICS],
    pub ioapic_count: usize,
    pub isos: [InterruptSourceOverride; MAX_ISOS],
    pub iso_count: usize,
}

impl MadtData {
    const EMPTY_CPU: ProcessorLocalApic = ProcessorLocalApic {
        acpi_processor_id: 0,
        apic_id: 0,
        enabled: false,
        online_capable: false,
    };
    const EMPTY_IOAPIC: IoApicInfo = IoApicInfo { id: 0, address: 0, gsi_base: 0 };
    const EMPTY_ISO: InterruptSourceOverride = InterruptSourceOverride {
        bus: 0,
        source_irq: 0,
        gsi: 0,
        polarity: Polarity::ConformsToBus,
        trigger: TriggerMode::ConformsToBus,
    };

    const fn empty() -> Self {
        Self {
            found: false,
            local_apic_address: 0,
            legacy_pic_present: false,
            cpus: [Self::EMPTY_CPU; MAX_CPUS],
            cpu_count: 0,
            ioapics: [Self::EMPTY_IOAPIC; MAX_IOAPICS],
            ioapic_count: 0,
            isos: [Self::EMPTY_ISO; MAX_ISOS],
            iso_count: 0,
        }
    }

    /// Looks up the Interrupt Source Override, if any, for a legacy
    /// ISA IRQ -- the GSI a given IOAPIC redirection entry needs to
    /// target, and the polarity/trigger mode it needs, are only ever
    /// the ISA defaults (GSI == irq, active-high, edge) when no
    /// override says otherwise (ACPI spec 5.2.12.5).
    #[allow(dead_code)]
    pub fn iso_for_irq(&self, irq: u8) -> Option<&InterruptSourceOverride> {
        self.isos[..self.iso_count]
            .iter()
            .find(|iso| iso.bus == 0 && iso.source_irq == irq)
    }
}

pub static MADT: crate::sync::Spinlock<MadtData> = crate::sync::Spinlock::new(MadtData::empty());

fn push_cpu(data: &mut MadtData, acpi_processor_id: u8, apic_id: u32, enabled: bool, online_capable: bool) {
    if !enabled && !online_capable {
        // Not present and not hot-pluggable later -- ACPI spec 5.2.12.2
        // says to ignore this entry entirely, not even count it as a
        // known-but-offline CPU.
        return;
    }
    if data.cpu_count >= MAX_CPUS {
        ulog!("MADT: dropping CPU (APIC ID {}) -- MAX_CPUS ({}) reached", apic_id, MAX_CPUS);
        return;
    }
    data.cpus[data.cpu_count] = ProcessorLocalApic { acpi_processor_id, apic_id, enabled, online_capable };
    data.cpu_count += 1;
}

/// Walks the MADT's variable-length entry list and stores the result
/// in `MADT`. `table_virt` is the MADT's SdtHeader start (already
/// checksum- and signature-validated by the caller, hal::acpi's
/// parse_acpi_table); `length` is that header's own `length` field,
/// i.e. the exact byte extent of the whole table.
pub fn parse(table_virt: usize, length: usize) {
    use crate::hal::acpi::Madt;
    use core::mem::size_of;

    if length < size_of::<Madt>() {
        ulog!("MADT: table shorter than its own fixed header, skipping entry walk");
        return;
    }

    let madt = unsafe { &*(table_virt as *const Madt) };
    let local_apic_address = unsafe { core::ptr::addr_of!(madt.local_apic_address).read_unaligned() };
    let flags = unsafe { core::ptr::addr_of!(madt.flags).read_unaligned() };

    let mut data = MadtData::empty();
    data.found = true;
    data.local_apic_address = local_apic_address as u64;
    data.legacy_pic_present = (flags & 1) != 0;

    let entries_start = table_virt + size_of::<Madt>();
    let entries_end = table_virt + length;
    let mut cursor = entries_start;

    while cursor + 2 <= entries_end {
        let entry_type = unsafe { *(cursor as *const u8) };
        let entry_length = unsafe { *((cursor + 1) as *const u8) } as usize;

        if entry_length < 2 || cursor + entry_length > entries_end {
            ulog!(
                "MADT: entry type {} has an invalid length ({}), stopping walk",
                entry_type,
                entry_length
            );
            break;
        }

        match entry_type {
            0 => {
                // Processor Local APIC: processor_id(u8) apic_id(u8) flags(u32)
                if entry_length >= 8 {
                    let acpi_processor_id = unsafe { *((cursor + 2) as *const u8) };
                    let apic_id = unsafe { *((cursor + 3) as *const u8) };
                    let pflags = unsafe { core::ptr::read_unaligned((cursor + 4) as *const u32) };
                    push_cpu(&mut data, acpi_processor_id, apic_id as u32, pflags & 1 != 0, pflags & 2 != 0);
                }
            }
            1 => {
                // I/O APIC: id(u8) reserved(u8) address(u32) gsi_base(u32)
                if entry_length >= 12 {
                    let id = unsafe { *((cursor + 2) as *const u8) };
                    let address = unsafe { core::ptr::read_unaligned((cursor + 4) as *const u32) };
                    let gsi_base = unsafe { core::ptr::read_unaligned((cursor + 8) as *const u32) };
                    if data.ioapic_count < MAX_IOAPICS {
                        data.ioapics[data.ioapic_count] = IoApicInfo { id, address, gsi_base };
                        data.ioapic_count += 1;
                    } else {
                        ulog!("MADT: dropping IO APIC id {} -- MAX_IOAPICS ({}) reached", id, MAX_IOAPICS);
                    }
                }
            }
            2 => {
                // Interrupt Source Override: bus(u8) source(u8) gsi(u32) flags(u16)
                if entry_length >= 10 {
                    let bus = unsafe { *((cursor + 2) as *const u8) };
                    let source_irq = unsafe { *((cursor + 3) as *const u8) };
                    let gsi = unsafe { core::ptr::read_unaligned((cursor + 4) as *const u32) };
                    let mps_flags = unsafe { core::ptr::read_unaligned((cursor + 8) as *const u16) };
                    let (polarity, trigger) = decode_mps_flags(mps_flags);
                    if data.iso_count < MAX_ISOS {
                        data.isos[data.iso_count] = InterruptSourceOverride { bus, source_irq, gsi, polarity, trigger };
                        data.iso_count += 1;
                    } else {
                        ulog!(
                            "MADT: dropping Interrupt Source Override for IRQ {} -- MAX_ISOS ({}) reached",
                            source_irq,
                            MAX_ISOS
                        );
                    }
                }
            }
            5 => {
                // Local APIC Address Override: reserved(u16) address(u64)
                if entry_length >= 12 {
                    let address = unsafe { core::ptr::read_unaligned((cursor + 4) as *const u64) };
                    data.local_apic_address = address;
                }
            }
            9 => {
                // Processor Local x2APIC: reserved(u16) x2apic_id(u32) flags(u32) acpi_uid(u32)
                if entry_length >= 16 {
                    let x2apic_id = unsafe { core::ptr::read_unaligned((cursor + 4) as *const u32) };
                    let pflags = unsafe { core::ptr::read_unaligned((cursor + 8) as *const u32) };
                    let acpi_uid = unsafe { core::ptr::read_unaligned((cursor + 12) as *const u32) };
                    push_cpu(&mut data, acpi_uid as u8, x2apic_id, pflags & 1 != 0, pflags & 2 != 0);
                }
            }
            _ => {
                // NMI Source (3), Local APIC NMI (4), and every entry
                // type ACPI has added since (Local x2APIC NMI 0xA,
                // GIC*-family entries used on ARM, etc.) -- none of
                // them affect discovering CPUs/IO-APICs, and
                // entry_length above already lets us skip past
                // whichever this is safely.
            }
        }

        cursor += entry_length;
    }

    ulog!(
        "MADT: {} usable CPU(s), {} IO-APIC(s), {} interrupt override(s), Local APIC @ 0x{:X}{}",
        data.cpu_count,
        data.ioapic_count,
        data.iso_count,
        data.local_apic_address,
        if data.legacy_pic_present { " (8259 PICs present)" } else { "" }
    );

    *MADT.lock() = data;
}
