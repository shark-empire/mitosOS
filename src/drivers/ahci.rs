//! AHCI (Advanced Host Controller Interface) SATA driver.
//!
//! Implements AHCI 1.3.1 closely enough to: perform BIOS/OS handoff,
//! enumerate every implemented port, initialize the command engine,
//! detect attached SATA devices, and perform polled 48-bit LBA sector
//! reads/writes plus IDENTIFY DEVICE / FLUSH CACHE EXT.
//!
//! # Integration
//! This driver never touches your frame allocator or page tables
//! directly. Instead it is generic over a [`Hal`] trait that *you*
//! implement once against your `memory`/`vmm` modules (mapping BAR5,
//! allocating DMA-safe memory, and translating buffer addresses). See
//! the worked example at the bottom of this file.
//!
//! # Quick start
//! ```ignore
//! let abar = PhysAddr::new(bar5_from_pci as u64);
//! let mut hal = YourHalImpl::new(/* ... */);
//! let mut ahci = unsafe { AhciController::new(abar, &mut hal)? };
//!
//! for port in ahci.iter_ports() {
//!     if port.kind() == DeviceKind::Sata {
//!         // port.sector_count(), port.supports_lba48(), ...
//!     }
//! }
//!
//! if let Some(disk) = ahci.port_mut(0) {
//!     let mut buf = [0u8; 512];
//!     disk.read_sectors(&mut hal, 0, &mut buf)?;
//! }
//! ```
//!
//! # Deliberately out of scope (documented, not forgotten)
//! - Native Command Queuing (NCQ): commands are issued one at a time and
//!   polled to completion. The register plumbing (PxSACT/PxCI, per-slot
//!   command tables) already supports extending this to multiple
//!   in-flight tags with interrupt-driven completion if you need it.
//! - ATAPI (CD/DVD) command sets.
//! - Hot-plug / staggered spin-up sequencing beyond SUD/POD.
//! - A full HBA reset (GHC.HR) - `new()` assumes firmware left the
//!   controller in a sane state, which is true on essentially all real
//!   hardware and QEMU/Bochs/VirtualBox's emulated AHCI.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
#[cfg(target_arch = "x86_64")]
use x86_64::{PhysAddr, VirtAddr};

// =========================================================================
// Hardware Abstraction Layer expected from the kernel
// =========================================================================

/// Everything this driver needs from your memory manager. Implement this
/// once against your real `memory`/`vmm` modules and pass it into
/// [`AhciController::new`] and every I/O call.
///
/// Keeping the driver generic over this trait means it has no opinion on
/// whether you use a physical-memory-offset mapping, per-page identity
/// mapping, or a full page-table mapper - that decision stays in your
/// memory manager, where it belongs.
pub trait Hal {
    /// Map `size` bytes of physical MMIO space at `phys` into the
    /// kernel's address space as device memory (no caching, no
    /// speculative access) and return the virtual base address. Called
    /// once, at controller creation.
    ///
    /// # Safety
    /// `phys` must be the real ABAR (BAR5) reported by PCI configuration
    /// space for this AHCI function, and must not already be mapped
    /// elsewhere as normal cacheable memory.
    unsafe fn map_mmio(&mut self, phys: PhysAddr, size: usize) -> VirtAddr;

    /// Allocate `size` bytes of physically-contiguous, zeroed,
    /// **4 KiB-aligned** memory suitable for DMA (command lists, the FIS
    /// receive area, command tables). 4 KiB alignment satisfies every
    /// AHCI alignment requirement (1 KiB command list, 256-byte FIS
    /// area, 128-byte command tables). Returns `None` if allocation
    /// fails.
    ///
    /// # Safety
    /// The returned region must stay mapped and exclusively owned by
    /// this driver for as long as the owning port is initialized.
    unsafe fn alloc_dma(&mut self, size: usize) -> Option<(PhysAddr, VirtAddr)>;

    /// Translate a virtual address the driver was handed (e.g. a
    /// caller's read/write buffer) into the physical address the HBA
    /// should DMA into/from. Called once per 4 KiB page touched by a
    /// transfer, so it must be cheap.
    ///
    /// # Safety
    /// `virt` must be currently mapped and backed by real physical
    /// memory for the lifetime of the in-flight command.
    unsafe fn virt_to_phys(&self, virt: VirtAddr) -> Option<PhysAddr>;

    /// Busy-wait for approximately `micros` microseconds, backed by
    /// whatever timer your kernel has (PIT/APIC/TSC). Used only for the
    /// small, bounded delays AHCI initialization and command completion
    /// require while polling.
    fn wait_micros(&self, micros: u32);
}

// =========================================================================
// Register offsets & bit definitions (AHCI 1.3.1)
// =========================================================================

// ---- Generic Host Control (offsets from ABAR) ----
const HBA_CAP: usize = 0x00;
const HBA_GHC: usize = 0x04;
const HBA_IS: usize = 0x08;
const HBA_PI: usize = 0x0C;
const HBA_VS: usize = 0x10;
const HBA_CAP2: usize = 0x24;
const HBA_BOHC: usize = 0x28;

const HBA_PORT_BASE: usize = 0x100;
const HBA_PORT_SIZE: usize = 0x80;
const HBA_MAX_PORTS: usize = 32;
/// MMIO span mapped for the whole HBA: generic registers + all 32
/// possible ports, rounded up to a clean 2-page (8 KiB) size.
const HBA_MMIO_SIZE: usize = 0x2000;

// GHC bits
const GHC_AE: u32 = 1 << 31; // AHCI Enable

// CAP bits
const CAP_NCS_SHIFT: u32 = 8;
const CAP_NCS_MASK: u32 = 0x1F; // bits 8-12: number of command slots - 1
const CAP_S64A: u32 = 1 << 31; // 64-bit addressing supported

// CAP2 / BOHC (BIOS/OS handoff)
const CAP2_BOH: u32 = 1 << 0;
const BOHC_BOS: u32 = 1 << 0; // BIOS Owned Semaphore
const BOHC_OOS: u32 = 1 << 1; // OS Owned Semaphore
const BOHC_BB: u32 = 1 << 4; // BIOS Busy

// ---- Port registers (offsets from a port's own base) ----
const PORT_CLB: usize = 0x00;
const PORT_CLBU: usize = 0x04;
const PORT_FB: usize = 0x08;
const PORT_FBU: usize = 0x0C;
const PORT_IS: usize = 0x10;
const PORT_IE: usize = 0x14;
const PORT_CMD: usize = 0x18;
const PORT_TFD: usize = 0x20;
const PORT_SIG: usize = 0x24;
const PORT_SSTS: usize = 0x28;
const PORT_SCTL: usize = 0x2C;
const PORT_SERR: usize = 0x30;
const PORT_SACT: usize = 0x34;
const PORT_CI: usize = 0x38;

// PxCMD bits
const PXCMD_ST: u32 = 1 << 0;
const PXCMD_SUD: u32 = 1 << 1;
const PXCMD_POD: u32 = 1 << 2;
const PXCMD_FRE: u32 = 1 << 4;
const PXCMD_FR: u32 = 1 << 14;
const PXCMD_CR: u32 = 1 << 15;

// PxTFD bits (device status byte lives in bits 0-7)
const ATA_STS_ERR: u32 = 1 << 0;
const ATA_STS_DRQ: u32 = 1 << 3;
const ATA_STS_BSY: u32 = 1 << 7;

// PxSSTS.DET
const SSTS_DET_MASK: u32 = 0xF;
const SSTS_DET_PRESENT: u32 = 3; // device present, PHY comm established

// Device signatures (PxSIG)
const SIG_ATA: u32 = 0x0000_0101;
const SIG_ATAPI: u32 = 0xEB14_0101;
const SIG_SEMB: u32 = 0xC33C_0101; // enclosure management bridge
const SIG_PM: u32 = 0x9669_0101; // port multiplier

// FIS types
const FIS_TYPE_REG_H2D: u8 = 0x27;

// ATA commands
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;
const ATA_CMD_IDENTIFY: u8 = 0xEC;

pub const SECTOR_SIZE: usize = 512;
const CMD_LIST_ENTRIES: usize = 32; // architectural max per AHCI spec
const PRDT_ENTRIES: usize = 9; // 8 usable pages + 1 headroom for misalignment
/// Largest single transfer this driver will build one command for.
/// `read_sectors`/`write_sectors` transparently loop for anything larger.
pub const MAX_TRANSFER_BYTES: usize = (PRDT_ENTRIES - 1) * 4096; // 32 KiB = 64 sectors
const CMD_TABLE_STRIDE: usize = 384; // >= size_of::<HbaCmdTable>(), 128-byte aligned
const FIS_RECV_SIZE: usize = 256;

// =========================================================================
// Errors
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhciError {
    /// BAR5 did not respond like an AHCI HBA (VS register sanity check).
    NotAnAhciController,
    /// PxSSTS.DET never reported an established link for this port.
    PortNotPresent,
    DeviceNotAta,
    EngineStopTimeout,
    EngineStartTimeout,
    FisReceiveStopTimeout,
    NoFreeCommandSlot,
    CommandTimeout,
    /// Raw PxTFD status/error bytes after a failed command.
    TaskFileError { status: u8, error: u8 },
    BufferNotSectorAligned,
    BufferTooLargeForOneCommand,
    DmaAllocationFailed,
    AddressTranslationFailed,
    /// Command list/FIS/table landed above 4 GiB but CAP.S64A is clear.
    Requires64BitDma,
}

// =========================================================================
// On-the-wire structures (must match hardware layout exactly - accessed
// only through raw pointers into DMA memory, never moved/copied as Rust
// values, so no Copy/Clone/Default derives are needed or attempted here)
// =========================================================================

#[repr(C)]
struct HbaCmdHeader {
    /// CFL(0-4) A(5) W(6) P(7) R(8) B(9) C(10) rsv(11) PMP(12-15) PRDTL(16-31)
    dw0: u32,
    prdbc: u32,
    ctba: u32,
    ctbau: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct HbaPrdtEntry {
    dba: u32,
    dbau: u32,
    reserved0: u32,
    /// bits 0-21: byte count - 1 (must be even). bit 31: interrupt on completion.
    dbc: u32,
}

#[repr(C)]
struct HbaCmdTable {
    cfis: [u8; 64],
    acmd: [u8; 16],
    reserved: [u8; 48],
    prdt: [HbaPrdtEntry; PRDT_ENTRIES],
}

/// Host-to-Device Register FIS - the only FIS type this driver sends.
#[repr(C)]
#[derive(Default)]
struct FisRegH2D {
    fis_type: u8,
    pm_port_c: u8, // bit 7 = "this is a Command" (vs Control)
    command: u8,
    featurel: u8,
    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8, // bit 6 = LBA mode
    lba3: u8,
    lba4: u8,
    lba5: u8,
    featureh: u8,
    countl: u8,
    counth: u8,
    icc: u8,
    control: u8,
    reserved: [u8; 4],
}

// =========================================================================
// Low-level MMIO register access
// =========================================================================

/// Thin, safe wrapper around a block of AHCI registers. Constructing one
/// touches nothing; `read`/`write` are the only unsafe operations, each
/// contained in its own block. The invariant that `base` points at real,
/// permanently-mapped device registers is established exactly once, by
/// whoever obtained it from [`Hal::map_mmio`].
#[derive(Clone, Copy)]
struct Regs {
    base: VirtAddr,
}

impl Regs {
    fn new(base: VirtAddr) -> Self {
        Self { base }
    }

    #[inline(always)]
    fn read(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.base.as_u64() as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn write(&self, offset: usize, value: u32) {
        unsafe { write_volatile((self.base.as_u64() as usize + offset) as *mut u32, value) }
    }

    fn set_bits(&self, offset: usize, mask: u32) {
        let v = self.read(offset);
        self.write(offset, v | mask);
    }

    fn clear_bits(&self, offset: usize, mask: u32) {
        let v = self.read(offset);
        self.write(offset, v & !mask);
    }
}

/// Poll `cond` until it returns `false` or `timeout_us` has elapsed,
/// sleeping `step_us` between checks via the HAL's timer. Returns `true`
/// if the condition cleared in time.
fn wait_while(hal: &impl Hal, cond: impl Fn() -> bool, timeout_us: u32, step_us: u32) -> bool {
    let mut waited = 0u32;
    while cond() {
        if waited >= timeout_us {
            return false;
        }
        hal.wait_micros(step_us);
        waited = waited.saturating_add(step_us);
    }
    true
}

// =========================================================================
// Devices
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Sata,
    Satapi,
    PortMultiplier,
    EnclosureManagement,
}

/// Parsed subset of ATA IDENTIFY DEVICE (word offsets per the ATA/ATAPI
/// command set spec).
pub struct AtaIdentify {
    pub sectors: u64,
    pub lba48: bool,
    pub model: [u8; 40],
}

impl AtaIdentify {
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model).unwrap_or("").trim()
    }
}

fn parse_identify(buf: &[u8; SECTOR_SIZE]) -> AtaIdentify {
    let word = |i: usize| -> u16 { u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) };

    // Word 83, bit 10: LBA48 supported.
    let lba48 = (word(83) & (1 << 10)) != 0;

    let sectors = if lba48 {
        // Words 100-103: 64-bit LBA48 total sector count.
        (word(100) as u64)
            | ((word(101) as u64) << 16)
            | ((word(102) as u64) << 32)
            | ((word(103) as u64) << 48)
    } else {
        // Words 60-61: 28-bit LBA total sector count.
        (word(60) as u64) | ((word(61) as u64) << 16)
    };

    // Words 27-46: model string, byte-swapped ASCII pairs.
    let mut model = [0u8; 40];
    for i in 0..20 {
        let w = word(27 + i);
        model[i * 2] = (w >> 8) as u8;
        model[i * 2 + 1] = w as u8;
    }

    AtaIdentify { sectors, lba48, model }
}

/// Runtime state for a single AHCI port after successful initialization.
pub struct AhciPort {
    index: u8,
    regs: Regs,
    clb_virt: VirtAddr,
    ctba_virt: VirtAddr,
    num_slots: u8,
    kind: DeviceKind,
    sectors: u64,
    lba48: bool,
}

impl AhciPort {
    pub fn index(&self) -> u8 {
        self.index
    }
    pub fn kind(&self) -> DeviceKind {
        self.kind
    }
    pub fn sector_count(&self) -> u64 {
        self.sectors
    }
    pub fn supports_lba48(&self) -> bool {
        self.lba48
    }

    // ---- DMA-region accessors -------------------------------------
    //
    // Safety invariant relied on throughout this impl block: for a given
    // slot, `cmd_header`/`cmd_table` are only ever both borrowed within a
    // single `issue_command` call, and `find_free_slot` + the PxCI
    // doorbell bit guarantee no two in-flight commands ever share a
    // slot. That is what makes manufacturing `&mut` references out of
    // raw DMA pointers here sound.

    fn cmd_header(&self, slot: u8) -> &mut HbaCmdHeader {
        unsafe { &mut *(self.clb_virt.as_u64() as *mut HbaCmdHeader).add(slot as usize) }
    }

    fn cmd_table(&self, slot: u8) -> &mut HbaCmdTable {
        let addr = self.ctba_virt.as_u64() as usize + (slot as usize) * CMD_TABLE_STRIDE;
        unsafe { &mut *(addr as *mut HbaCmdTable) }
    }

    fn find_free_slot(&self) -> Result<u8, AhciError> {
        let busy = self.regs.read(PORT_SACT) | self.regs.read(PORT_CI);
        (0..self.num_slots)
            .find(|slot| busy & (1 << slot) == 0)
            .ok_or(AhciError::NoFreeCommandSlot)
    }

    /// Build up to `PRDT_ENTRIES` PRDT entries covering `buf`, one entry
    /// per (up to) 4 KiB physical page, translating each page through
    /// the HAL. Returns the number of entries used.
    fn build_prdt(table: &mut HbaCmdTable, buf: &mut [u8], hal: &impl Hal) -> Result<u16, AhciError> {
        if buf.len() > MAX_TRANSFER_BYTES {
            return Err(AhciError::BufferTooLargeForOneCommand);
        }

        let mut count = 0usize;
        let mut offset = 0usize;
        while offset < buf.len() {
            if count >= PRDT_ENTRIES {
                return Err(AhciError::BufferTooLargeForOneCommand);
            }

            let virt = VirtAddr::new(buf.as_ptr() as u64 + offset as u64);
            let page_off = (virt.as_u64() % 4096) as usize;
            let chunk = core::cmp::min(4096 - page_off, buf.len() - offset);

            let phys = unsafe { hal.virt_to_phys(virt) }.ok_or(AhciError::AddressTranslationFailed)?;

            let prdt = &mut table.prdt[count];
            prdt.dba = phys.as_u64() as u32;
            prdt.dbau = (phys.as_u64() >> 32) as u32;
            prdt.reserved0 = 0;
            prdt.dbc = ((chunk as u32 - 1) & 0x003F_FFFF) | (1 << 31); // I bit

            offset += chunk;
            count += 1;
        }

        Ok(count as u16)
    }

    /// Build, issue, and poll a single Register H2D command to completion.
    /// `buf` may be empty for commands with no data phase (e.g. FLUSH).
    fn issue_command(
        &mut self,
        hal: &mut impl Hal,
        command: u8,
        lba: u64,
        sector_count: u16,
        write: bool,
        buf: &mut [u8],
    ) -> Result<(), AhciError> {
        if !wait_while(
            hal,
            || self.regs.read(PORT_TFD) & (ATA_STS_BSY | ATA_STS_DRQ) != 0,
            500_000,
            1000,
        ) {
            return Err(AhciError::CommandTimeout);
        }

        let slot = self.find_free_slot()?;

        let table = self.cmd_table(slot);
        let prdt_count = if buf.is_empty() {
            0
        } else {
            Self::build_prdt(table, buf, hal)?
        };

        let fis_dwords = (core::mem::size_of::<FisRegH2D>() / 4) as u32; // = 5
        let header = self.cmd_header(slot);
        header.dw0 = fis_dwords | ((write as u32) << 6) | ((prdt_count as u32) << 16);
        header.prdbc = 0;

        let cfis = unsafe { &mut *(table.cfis.as_mut_ptr() as *mut FisRegH2D) };
        *cfis = FisRegH2D::default();
        cfis.fis_type = FIS_TYPE_REG_H2D;
        cfis.pm_port_c = 1 << 7; // command, not control
        cfis.command = command;
        cfis.device = 1 << 6; // LBA mode
        cfis.lba0 = lba as u8;
        cfis.lba1 = (lba >> 8) as u8;
        cfis.lba2 = (lba >> 16) as u8;
        cfis.lba3 = (lba >> 24) as u8;
        cfis.lba4 = (lba >> 32) as u8;
        cfis.lba5 = (lba >> 40) as u8;
        cfis.countl = sector_count as u8;
        cfis.counth = (sector_count >> 8) as u8;

        // The HBA is an independent bus master; make sure every write
        // above is globally visible before ringing the doorbell.
        fence(Ordering::SeqCst);
        self.regs.write(PORT_CI, 1 << slot);

        if !wait_while(hal, || self.regs.read(PORT_CI) & (1 << slot) != 0, 5_000_000, 1000) {
            return Err(AhciError::CommandTimeout);
        }

        let tfd = self.regs.read(PORT_TFD);
        if tfd & ATA_STS_ERR != 0 {
            return Err(AhciError::TaskFileError {
                status: tfd as u8,
                error: (tfd >> 8) as u8,
            });
        }

        Ok(())
    }

    /// Send ATA IDENTIFY DEVICE and parse the response. `init_port` calls
    /// this once at startup and caches the result in `sector_count()` /
    /// `supports_lba48()`; call it again yourself if you need the model
    /// string or want to re-check after a reset.
    pub fn identify(&mut self, hal: &mut impl Hal) -> Result<AtaIdentify, AhciError> {
        let mut buf = [0u8; SECTOR_SIZE];
        self.issue_command(hal, ATA_CMD_IDENTIFY, 0, 1, false, &mut buf)?;
        Ok(parse_identify(&buf))
    }

    /// FLUSH CACHE EXT - call after a batch of writes you need durable
    /// before acknowledging e.g. an fsync.
    pub fn flush(&mut self, hal: &mut impl Hal) -> Result<(), AhciError> {
        self.issue_command(hal, ATA_CMD_FLUSH_CACHE_EXT, 0, 0, false, &mut [])
    }

    /// Read `buf.len() / SECTOR_SIZE` sectors starting at `lba` into
    /// `buf`. `buf.len()` must be a non-zero multiple of [`SECTOR_SIZE`];
    /// transfers larger than [`MAX_TRANSFER_BYTES`] are chunked
    /// automatically across multiple commands.
    pub fn read_sectors(&mut self, hal: &mut impl Hal, lba: u64, buf: &mut [u8]) -> Result<(), AhciError> {
        self.rw_sectors(hal, lba, buf, false)
    }

    /// Write `buf.len() / SECTOR_SIZE` sectors starting at `lba` from `buf`.
    pub fn write_sectors(&mut self, hal: &mut impl Hal, lba: u64, buf: &mut [u8]) -> Result<(), AhciError> {
        self.rw_sectors(hal, lba, buf, true)
    }

    fn rw_sectors(&mut self, hal: &mut impl Hal, lba: u64, buf: &mut [u8], write: bool) -> Result<(), AhciError> {
        if self.kind != DeviceKind::Sata {
            return Err(AhciError::DeviceNotAta);
        }
        if buf.is_empty() || buf.len() % SECTOR_SIZE != 0 {
            return Err(AhciError::BufferNotSectorAligned);
        }

        let command = if write { ATA_CMD_WRITE_DMA_EXT } else { ATA_CMD_READ_DMA_EXT };
        let mut done = 0usize;
        let mut cur_lba = lba;
        while done < buf.len() {
            let chunk_len = core::cmp::min(buf.len() - done, MAX_TRANSFER_BYTES);
            let sectors = (chunk_len / SECTOR_SIZE) as u16;
            self.issue_command(hal, command, cur_lba, sectors, write, &mut buf[done..done + chunk_len])?;
            done += chunk_len;
            cur_lba += sectors as u64;
        }
        Ok(())
    }
}

// =========================================================================
// Controller
// =========================================================================

pub struct AhciController {
    hba: Regs,
    num_slots: u8,
    supports_64bit_dma: bool,
    ports: [Option<AhciPort>; HBA_MAX_PORTS],
}

impl AhciController {
    /// Bring up the AHCI controller whose ABAR (BAR5) is at `abar_phys`:
    /// performs BIOS/OS handoff, sets AHCI Enable, walks the Ports
    /// Implemented bitmap, and fully rebases + starts every port that
    /// has a device attached.
    ///
    /// # Safety
    /// `abar_phys` must be the genuine ABAR reported by PCI configuration
    /// space for an AHCI-class (0x01/0x06) function - an incorrect
    /// address here means the driver will read/write arbitrary physical
    /// memory as if it were device registers.
    pub unsafe fn new(abar_phys: PhysAddr, hal: &mut impl Hal) -> Result<Self, AhciError> {
        let mmio = unsafe { hal.map_mmio(abar_phys, HBA_MMIO_SIZE) };
        let hba = Regs::new(mmio);

        // Sanity check: the version register is never zero on real
        // hardware; all-ones generally means the BAR/mapping is wrong.
        let vs = hba.read(HBA_VS);
        if vs == 0 || vs == 0xFFFF_FFFF {
            return Err(AhciError::NotAnAhciController);
        }

        Self::bios_os_handoff(hba, hal);

        hba.set_bits(HBA_GHC, GHC_AE);

        let cap = hba.read(HBA_CAP);
        let pi = hba.read(HBA_PI);

        let num_slots = (((cap >> CAP_NCS_SHIFT) & CAP_NCS_MASK) + 1) as u8;
        let supports_64bit_dma = (cap & CAP_S64A) != 0;

        let mut controller = Self {
            hba,
            num_slots,
            supports_64bit_dma,
            ports: core::array::from_fn(|_| None),
        };

        for i in 0..HBA_MAX_PORTS {
            if (pi & (1 << i)) == 0 {
                continue; // not implemented on this controller
            }
            // A port that fails to come up (nothing plugged in, stuck
            // busy, allocation failure, ...) shouldn't fail the whole
            // controller - it's just left as `None`.
            controller.ports[i] = controller.init_port(i as u8, hal).ok();
        }

        Ok(controller)
    }

    /// AHCI spec 10.6.3: hand HBA ownership from firmware to the OS
    /// before touching anything else, when the controller supports it.
    fn bios_os_handoff(hba: Regs, hal: &impl Hal) {
        if hba.read(HBA_CAP2) & CAP2_BOH == 0 {
            return; // handoff not supported/required by this controller
        }
        hba.set_bits(HBA_BOHC, BOHC_OOS);
        wait_while(hal, || hba.read(HBA_BOHC) & BOHC_BOS != 0, 25_000, 1000);
        if hba.read(HBA_BOHC) & BOHC_BB != 0 {
            // Firmware is still mid-transaction; the spec's suggested
            // fallback is to just wait it out before proceeding.
            hal.wait_micros(2_000_000);
        }
    }

    fn init_port(&self, index: u8, hal: &mut impl Hal) -> Result<AhciPort, AhciError> {
        let port_base = VirtAddr::new(
            self.hba.base.as_u64() + HBA_PORT_BASE as u64 + (index as u64) * HBA_PORT_SIZE as u64,
        );
        let regs = Regs::new(port_base);

        let ssts = regs.read(PORT_SSTS);
        if (ssts & SSTS_DET_MASK) != SSTS_DET_PRESENT {
            return Err(AhciError::PortNotPresent);
        }

        Self::stop_cmd_engine(regs, hal)?;

        // Command list: 32 headers x 32 bytes = 1 KiB.
        let (clb_phys, clb_virt) =
            unsafe { hal.alloc_dma(CMD_LIST_ENTRIES * 32) }.ok_or(AhciError::DmaAllocationFailed)?;
        // FIS receive area: 256 bytes. Allocated for spec compliance;
        // this polling-mode driver reads completion status straight
        // from PxTFD/PxSIG rather than parsing FISes out of it.
        let (fis_phys, fis_virt) = unsafe { hal.alloc_dma(FIS_RECV_SIZE) }.ok_or(AhciError::DmaAllocationFailed)?;
        // One command table per slot, packed at a fixed, 128-byte-aligned stride.
        let ctba_size = self.num_slots as usize * CMD_TABLE_STRIDE;
        let (ctba_phys, ctba_virt) = unsafe { hal.alloc_dma(ctba_size) }.ok_or(AhciError::DmaAllocationFailed)?;
        let _ = fis_virt; // kept alive via fis_phys programmed below; unused otherwise

        if !self.supports_64bit_dma
            && (clb_phys.as_u64() > u32::MAX as u64
                || fis_phys.as_u64() > u32::MAX as u64
                || ctba_phys.as_u64() > u32::MAX as u64)
        {
            return Err(AhciError::Requires64BitDma);
        }

        for slot in 0..self.num_slots as usize {
            let hdr = unsafe { &mut *(clb_virt.as_u64() as *mut HbaCmdHeader).add(slot) };
            let ctba = ctba_phys.as_u64() + (slot * CMD_TABLE_STRIDE) as u64;
            hdr.dw0 = 0;
            hdr.prdbc = 0;
            hdr.ctba = ctba as u32;
            hdr.ctbau = (ctba >> 32) as u32;
        }

        regs.write(PORT_CLB, clb_phys.as_u64() as u32);
        regs.write(PORT_CLBU, (clb_phys.as_u64() >> 32) as u32);
        regs.write(PORT_FB, fis_phys.as_u64() as u32);
        regs.write(PORT_FBU, (fis_phys.as_u64() >> 32) as u32);

        // Clear any stale error/interrupt status left by firmware.
        regs.write(PORT_SERR, regs.read(PORT_SERR));
        regs.write(PORT_IS, regs.read(PORT_IS));

        // Power up the port / spin up the device where the HBA supports
        // it; writes to unsupported bits are simply ignored by hardware.
        regs.set_bits(PORT_CMD, PXCMD_SUD | PXCMD_POD | PXCMD_FRE);

        // Give the device a moment to stop reporting BSY/DRQ before we
        // start the command engine on top of it. Not fatal if it
        // doesn't clear here - some devices only settle once ST is set.
        wait_while(hal, || regs.read(PORT_TFD) & (ATA_STS_BSY | ATA_STS_DRQ) != 0, 1_000_000, 1000);

        Self::start_cmd_engine(regs, hal)?;

        let sig = regs.read(PORT_SIG);
        let kind = match sig {
            SIG_ATAPI => DeviceKind::Satapi,
            SIG_SEMB => DeviceKind::EnclosureManagement,
            SIG_PM => DeviceKind::PortMultiplier,
            _ => DeviceKind::Sata, // SIG_ATA, or still settling - treat as ATA
        };

        let mut port = AhciPort {
            index,
            regs,
            clb_virt,
            ctba_virt,
            num_slots: self.num_slots,
            kind,
            sectors: 0,
            lba48: false,
        };

        if port.kind == DeviceKind::Sata {
            if let Ok(id) = port.identify(hal) {
                port.sectors = id.sectors;
                port.lba48 = id.lba48;
            }
        }

        Ok(port)
    }

    fn stop_cmd_engine(regs: Regs, hal: &impl Hal) -> Result<(), AhciError> {
        if regs.read(PORT_CMD) & PXCMD_ST != 0 {
            regs.clear_bits(PORT_CMD, PXCMD_ST);
        }
        if !wait_while(hal, || regs.read(PORT_CMD) & PXCMD_CR != 0, 500_000, 1000) {
            return Err(AhciError::EngineStopTimeout);
        }

        if regs.read(PORT_CMD) & PXCMD_FRE != 0 {
            regs.clear_bits(PORT_CMD, PXCMD_FRE);
        }
        if !wait_while(hal, || regs.read(PORT_CMD) & PXCMD_FR != 0, 500_000, 1000) {
            return Err(AhciError::FisReceiveStopTimeout);
        }
        Ok(())
    }

    fn start_cmd_engine(regs: Regs, hal: &impl Hal) -> Result<(), AhciError> {
        if !wait_while(hal, || regs.read(PORT_CMD) & PXCMD_CR != 0, 500_000, 1000) {
            return Err(AhciError::EngineStartTimeout);
        }
        regs.set_bits(PORT_CMD, PXCMD_FRE);
        regs.set_bits(PORT_CMD, PXCMD_ST);
        Ok(())
    }

    pub fn max_command_slots(&self) -> u8 {
        self.num_slots
    }

    pub fn supports_64bit_dma(&self) -> bool {
        self.supports_64bit_dma
    }

    pub fn port(&self, index: u8) -> Option<&AhciPort> {
        self.ports.get(index as usize)?.as_ref()
    }

    pub fn port_mut(&mut self, index: u8) -> Option<&mut AhciPort> {
        self.ports.get_mut(index as usize)?.as_mut()
    }

    pub fn iter_ports(&self) -> impl Iterator<Item = &AhciPort> {
        self.ports.iter().filter_map(|p| p.as_ref())
    }
}

// =========================================================================
// Example Hal implementation - ADAPT THIS to your real memory/vmm modules.
// Shown assuming the common "everything is offset-mapped" scheme (the
// bootloader/your paging setup identity-maps all of physical RAM at a
// fixed `PHYS_MEM_OFFSET`); swap the bodies for real calls into your
// frame allocator if you use a different scheme.
// =========================================================================
//
// pub struct OffsetHal<'a> {
//     pub phys_mem_offset: u64,
//     pub frame_allocator: &'a mut crate::memory::BootInfoFrameAllocator,
// }
//
// impl<'a> Hal for OffsetHal<'a> {
//     unsafe fn map_mmio(&mut self, phys: PhysAddr, _size: usize) -> VirtAddr {
//         // MMIO needs no separate mapping step if physical memory is
//         // already fully offset-mapped - same translation as RAM.
//         VirtAddr::new(phys.as_u64() + self.phys_mem_offset)
//     }
//
//     unsafe fn alloc_dma(&mut self, size: usize) -> Option<(PhysAddr, VirtAddr)> {
//         let frames_needed = (size + 4095) / 4096;
//         // Replace with your allocator's "N contiguous frames" call.
//         let start_phys = self.frame_allocator.allocate_contiguous(frames_needed)?;
//         let virt = VirtAddr::new(start_phys.as_u64() + self.phys_mem_offset);
//         core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, frames_needed * 4096);
//         Some((start_phys, virt))
//     }
//
//     unsafe fn virt_to_phys(&self, virt: VirtAddr) -> Option<PhysAddr> {
//         Some(PhysAddr::new(virt.as_u64() - self.phys_mem_offset))
//     }
//
//     fn wait_micros(&self, micros: u32) {
//         // Prefer a real timer (PIT/APIC/TSC) if your kernel has one by
//         // this point; this is a crude, uncalibrated fallback.
//         for _ in 0..(micros as u64 * 1000) {
//             core::hint::spin_loop();
//         }
//     }
// }