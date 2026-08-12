//! AHCI (Advanced Host Controller Interface) SATA driver.
//!
//! Implements AHCI 1.3.1 with interrupt-driven DMA completion, 
//! BIOS/OS handoff, port enumeration, and 48-bit LBA sector reads/writes.

#[cfg(target_arch = "x86_64")]
use x86_64::structures::idt::InterruptStackFrame;


use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};
use crate::addr::{PhysAddr, VirtAddr};

// =========================================================================
// Global IRQ Completion State
// =========================================================================

/// Wait-queue state array for active slots (32 Command Slots per port)
static SLOT_COMPLETION: [AtomicBool; 32] = [const { AtomicBool::new(false) }; 32];

/// Software-tracked bitmask of commands currently pending execution
static PENDING_SLOTS: AtomicU32 = AtomicU32::new(0);

// =========================================================================
// Hardware Abstraction Layer
// =========================================================================

pub trait Hal {
    unsafe fn map_mmio(&mut self, phys: PhysAddr, size: usize) -> VirtAddr;
    unsafe fn alloc_dma(&mut self, size: usize) -> Option<(PhysAddr, VirtAddr)>;
    unsafe fn virt_to_phys(&self, virt: VirtAddr) -> Option<PhysAddr>;
    fn wait_micros(&self, micros: u32);
}

// =========================================================================
// Register offsets & bit definitions (AHCI 1.3.1)
// =========================================================================

const HBA_CAP: usize       = 0x00;
const HBA_GHC: usize       = 0x04;
const HBA_IS: usize        = 0x08;
const HBA_PI: usize        = 0x0C;
const HBA_VS: usize        = 0x10;
const HBA_CAP2: usize      = 0x24;
const HBA_BOHC: usize      = 0x28;

const HBA_PORT_BASE: usize = 0x100;
const HBA_PORT_SIZE: usize = 0x80;
const HBA_MAX_PORTS: usize = 32;
const HBA_MMIO_SIZE: usize = 0x2000;

// GHC bits
const GHC_IE: u32          = 1 << 1;  // Global Interrupt Enable
const GHC_AE: u32          = 1 << 31; // AHCI Enable

// CAP bits
const CAP_NCS_SHIFT: u32   = 8;
const CAP_NCS_MASK: u32    = 0x1F;
const CAP_S64A: u32        = 1 << 31;

// CAP2 / BOHC
const CAP2_BOH: u32        = 1 << 0;
const BOHC_BOS: u32        = 1 << 0;
const BOHC_OOS: u32        = 1 << 1;
const BOHC_BB: u32         = 1 << 4;

// Port registers
const PORT_CLB: usize      = 0x00;
const PORT_CLBU: usize     = 0x04;
const PORT_FB: usize       = 0x08;
const PORT_FBU: usize      = 0x0C;
const PORT_IS: usize       = 0x10;
const PORT_IE: usize       = 0x14;
const PORT_CMD: usize      = 0x18;
const PORT_TFD: usize      = 0x20;
const PORT_SIG: usize      = 0x24;
const PORT_SSTS: usize     = 0x28;
const PORT_SCTL: usize     = 0x2C;
const PORT_SERR: usize     = 0x30;
const PORT_SACT: usize     = 0x34;
const PORT_CI: usize       = 0x38;

// PxCMD bits
const PXCMD_ST: u32        = 1 << 0;
const PXCMD_SUD: u32       = 1 << 1;
const PXCMD_POD: u32       = 1 << 2;
const PXCMD_FRE: u32       = 1 << 4;
const PXCMD_FR: u32        = 1 << 14;
const PXCMD_CR: u32        = 1 << 15;

// PxIE bits
const PORT_IE_DHRS: u32    = 1 << 0;  // Device to Host Register FIS Interrupt Enable

// PxTFD bits
const ATA_STS_ERR: u32     = 1 << 0;
const ATA_STS_DRQ: u32     = 1 << 3;
const ATA_STS_BSY: u32     = 1 << 7;

// PxSSTS.DET
const SSTS_DET_MASK: u32   = 0xF;
const SSTS_DET_PRESENT: u32 = 3;

// Signatures
const SIG_ATA: u32         = 0x0000_0101;
const SIG_ATAPI: u32       = 0xEB14_0101;
const SIG_SEMB: u32        = 0xC33C_0101;
const SIG_PM: u32          = 0x9669_0101;

// FIS & Commands
const FIS_TYPE_REG_H2D: u8 = 0x27;
const ATA_CMD_READ_DMA_EXT: u8  = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;
const ATA_CMD_IDENTIFY: u8      = 0xEC;

pub const SECTOR_SIZE: usize    = 512;
const CMD_LIST_ENTRIES: usize   = 32;
const PRDT_ENTRIES: usize       = 9;
pub const MAX_TRANSFER_BYTES: usize = (PRDT_ENTRIES - 1) * 4096;
const CMD_TABLE_STRIDE: usize  = 384;
const FIS_RECV_SIZE: usize     = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhciError {
    NotAnAhciController,
    PortNotPresent,
    DeviceNotAta,
    EngineStopTimeout,
    EngineStartTimeout,
    FisReceiveStopTimeout,
    NoFreeCommandSlot,
    CommandTimeout,
    TaskFileError { status: u8, error: u8 },
    BufferNotSectorAligned,
    BufferTooLargeForOneCommand,
    DmaAllocationFailed,
    AddressTranslationFailed,
    Requires64BitDma,
}

#[repr(C)]
struct HbaCmdHeader {
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
    dbc: u32,
}

#[repr(C)]
struct HbaCmdTable {
    cfis: [u8; 64],
    acmd: [u8; 16],
    reserved: [u8; 48],
    prdt: [HbaPrdtEntry; PRDT_ENTRIES],
}

#[repr(C)]
#[derive(Default)]
struct FisRegH2D {
    fis_type: u8,
    pm_port_c: u8,
    command: u8,
    featurel: u8,
    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8,
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

#[derive(Clone, Copy)]
struct Regs {
    base: VirtAddr,
}

impl Regs {
    fn new(base: VirtAddr) -> Self { Self { base } }

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

fn wait_while(hal: &impl Hal, cond: impl Fn() -> bool, timeout_us: u32, step_us: u32) -> bool {
    let mut waited = 0u32;
    while cond() {
        if waited >= timeout_us { return false; }
        hal.wait_micros(step_us);
        waited = waited.saturating_add(step_us);
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Sata,
    Satapi,
    PortMultiplier,
    EnclosureManagement,
}

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
    let lba48 = (word(83) & (1 << 10)) != 0;
    let sectors = if lba48 {
        (word(100) as u64) | ((word(101) as u64) << 16) | ((word(102) as u64) << 32) | ((word(103) as u64) << 48)
    } else {
        (word(60) as u64) | ((word(61) as u64) << 16)
    };
    let mut model = [0u8; 40];
    for i in 0..20 {
        let w = word(27 + i);
        model[i * 2] = (w >> 8) as u8;
        model[i * 2 + 1] = w as u8;
    }
    AtaIdentify { sectors, lba48, model }
}

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
    pub fn index(&self) -> u8 { self.index }
    pub fn kind(&self) -> DeviceKind { self.kind }
    pub fn sector_count(&self) -> u64 { self.sectors }
    pub fn supports_lba48(&self) -> bool { self.lba48 }

    /// Enable interrupt generation for this port
    pub fn enable_interrupts(&mut self) {
        self.regs.write(PORT_IE, PORT_IE_DHRS);
    }

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
            prdt.dbc = ((chunk as u32 - 1) & 0x003F_FFFF) | (1 << 31);

            offset += chunk;
            count += 1;
        }

        Ok(count as u16)
    }

    fn issue_command(
        &mut self,
        hal: &mut impl Hal,
        command: u8,
        lba: u64,
        sector_count: u16,
        write: bool,
        buf: &mut [u8],
    ) -> Result<(), AhciError> {
        if !wait_while(hal, || self.regs.read(PORT_TFD) & (ATA_STS_BSY | ATA_STS_DRQ) != 0, 500_000, 1000) {
            return Err(AhciError::CommandTimeout);
        }

        let slot = self.find_free_slot()?;

        // Reset IRQ completion tracker for this slot
        SLOT_COMPLETION[slot as usize].store(false, Ordering::SeqCst);
        
        // Mark slot as pending in software tracker before issuing to hardware
        PENDING_SLOTS.fetch_or(1 << (slot as u32), Ordering::SeqCst);

        let table = self.cmd_table(slot);
        let prdt_count = if buf.is_empty() { 0 } else { Self::build_prdt(table, buf, hal)? };

        let fis_dwords = (core::mem::size_of::<FisRegH2D>() / 4) as u32;
        let header = self.cmd_header(slot);
        header.dw0 = fis_dwords | ((write as u32) << 6) | ((prdt_count as u32) << 16);
        header.prdbc = 0;

        let cfis = unsafe { &mut *(table.cfis.as_mut_ptr() as *mut FisRegH2D) };
        *cfis = FisRegH2D::default();
        cfis.fis_type = FIS_TYPE_REG_H2D;
        cfis.pm_port_c = 1 << 7;
        cfis.command = command;
        cfis.device = 1 << 6;
        cfis.lba0 = lba as u8;
        cfis.lba1 = (lba >> 8) as u8;
        cfis.lba2 = (lba >> 16) as u8;
        cfis.lba3 = (lba >> 24) as u8;
        cfis.lba4 = (lba >> 32) as u8;
        cfis.lba5 = (lba >> 40) as u8;
        cfis.countl = sector_count as u8;
        cfis.counth = (sector_count >> 8) as u8;

        fence(Ordering::SeqCst);
        self.regs.write(PORT_CI, 1 << slot);

        // Wait non-blockingly via IRQ signal feedback instead of continuous port status polling
        while !SLOT_COMPLETION[slot as usize].load(Ordering::Acquire) {
            core::hint::spin_loop();
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

    pub fn identify(&mut self, hal: &mut impl Hal) -> Result<AtaIdentify, AhciError> {
        let mut buf = [0u8; SECTOR_SIZE];
        self.issue_command(hal, ATA_CMD_IDENTIFY, 0, 1, false, &mut buf)?;
        Ok(parse_identify(&buf))
    }

    pub fn flush(&mut self, hal: &mut impl Hal) -> Result<(), AhciError> {
        self.issue_command(hal, ATA_CMD_FLUSH_CACHE_EXT, 0, 0, false, &mut [])
    }

    pub fn read_sectors(&mut self, hal: &mut impl Hal, lba: u64, buf: &mut [u8]) -> Result<(), AhciError> {
        self.rw_sectors(hal, lba, buf, false)
    }

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

pub struct AhciController {
    hba: Regs,
    num_slots: u8,
    supports_64bit_dma: bool,
    ports: [Option<AhciPort>; HBA_MAX_PORTS],
}

impl AhciController {
    pub unsafe fn new(abar_phys: PhysAddr, hal: &mut impl Hal) -> Result<Self, AhciError> {
        let mmio = unsafe { hal.map_mmio(abar_phys, HBA_MMIO_SIZE) };
        let hba = Regs::new(mmio);

        let vs = hba.read(HBA_VS);
        if vs == 0 || vs == 0xFFFF_FFFF {
            return Err(AhciError::NotAnAhciController);
        }

        Self::bios_os_handoff(hba, hal);

        // Enable Global Host Interrupts + AHCI Engine
        hba.set_bits(HBA_GHC, GHC_AE | GHC_IE);

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
                continue;
            }
            if let Ok(mut port) = controller.init_port(i as u8, hal) {
                port.enable_interrupts();
                controller.ports[i] = Some(port);
            }
        }

        Ok(controller)
    }

    fn bios_os_handoff(hba: Regs, hal: &impl Hal) {
        if hba.read(HBA_CAP2) & CAP2_BOH == 0 {
            return;
        }
        hba.set_bits(HBA_BOHC, BOHC_OOS);
        wait_while(hal, || hba.read(HBA_BOHC) & BOHC_BOS != 0, 25_000, 1000);
        if hba.read(HBA_BOHC) & BOHC_BB != 0 {
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

        let (clb_phys, clb_virt) =
            unsafe { hal.alloc_dma(CMD_LIST_ENTRIES * 32) }.ok_or(AhciError::DmaAllocationFailed)?;
        let (fis_phys, _fis_virt) = unsafe { hal.alloc_dma(FIS_RECV_SIZE) }.ok_or(AhciError::DmaAllocationFailed)?;
        let ctba_size = self.num_slots as usize * CMD_TABLE_STRIDE;
        let (ctba_phys, ctba_virt) = unsafe { hal.alloc_dma(ctba_size) }.ok_or(AhciError::DmaAllocationFailed)?;

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

        regs.write(PORT_SERR, regs.read(PORT_SERR));
        regs.write(PORT_IS, regs.read(PORT_IS));

        regs.set_bits(PORT_CMD, PXCMD_SUD | PXCMD_POD | PXCMD_FRE);

        wait_while(hal, || regs.read(PORT_TFD) & (ATA_STS_BSY | ATA_STS_DRQ) != 0, 1_000_000, 1000);

        Self::start_cmd_engine(regs, hal)?;

        let sig = regs.read(PORT_SIG);
        let kind = match sig {
            SIG_ATAPI => DeviceKind::Satapi,
            SIG_SEMB => DeviceKind::EnclosureManagement,
            SIG_PM => DeviceKind::PortMultiplier,
            _ => DeviceKind::Sata,
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

    pub fn max_command_slots(&self) -> u8 { self.num_slots }
    pub fn supports_64bit_dma(&self) -> bool { self.supports_64bit_dma }
    pub fn port(&self, index: u8) -> Option<&AhciPort> { self.ports.get(index as usize)?.as_ref() }
    pub fn port_mut(&mut self, index: u8) -> Option<&mut AhciPort> { self.ports.get_mut(index as usize)?.as_mut() }
    pub fn iter_ports(&self) -> impl Iterator<Item = &AhciPort> { self.ports.iter().filter_map(|p| p.as_ref()) }
}

/// Global Top-Level AHCI Interrupt Handler for x86_64 (Registered in IDT)
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn ahci_irq_handler(_frame: x86_64::structures::idt::InterruptStackFrame) {
    // See memory::phys_to_virt's doc comment: no permanent identity
    // map on x86_64, so this can't be a compile-time constant --
    // Limine's HHDM offset is bootloader-chosen and varies.
    let ahci_base = crate::memory::phys_to_virt(0x4000_0000) as *mut u32; // Active HBA MMIO Higher-Half Base
    
    unsafe {
        let is_ptr = ahci_base.add(HBA_IS / 4);
        let active_ports = read_volatile(is_ptr);

        if active_ports & 1 != 0 {
            let port0_base = ahci_base.add(0x100 / 4);
            let port_is_ptr = port0_base.add(PORT_IS / 4);
            let interrupt_status = read_volatile(port_is_ptr);

            // Write 1 to clear port interrupt status
            write_volatile(port_is_ptr, interrupt_status);

            let port_ci_ptr = port0_base.add(PORT_CI / 4);
            let active_slots = read_volatile(port_ci_ptr);

            // Safely determine completion by comparing software-pending state against hardware PORT_CI
            let pending = PENDING_SLOTS.load(Ordering::Acquire);
            let completed = pending & !active_slots;
            
            if completed != 0 {
                PENDING_SLOTS.fetch_and(!completed, Ordering::Release);
                for slot in 0..32 {
                    if (completed & (1 << slot)) != 0 {
                        SLOT_COMPLETION[slot].store(true, Ordering::Release);
                    }
                }
            }
        }

        // Send End of Interrupt (EOI) to Local APIC. 0xFEE000B0 (LAPIC
        // base 0xFEE00000 + EOI register offset 0xB0) is an x86
        // architectural constant, true on every system regardless of
        // bootloader -- only the higher-half offset needs translating.
        let lapic_eoi = crate::memory::phys_to_virt(0xFEE0_00B0) as *mut u32;
        write_volatile(lapic_eoi, 0);
    }
}


/// Global Top-Level AHCI Interrupt Handler for AArch64 (GIC / Standard C ABI)
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub extern "C" fn ahci_irq_handler() {
    // AArch64 GIC IRQ handling logic
    let ahci_base = 0xFFFF_8000_4000_0000 as *mut u32;

    unsafe {
        let is_ptr = ahci_base.add(HBA_IS / 4);
        let active_ports = read_volatile(is_ptr);

        if active_ports & 1 != 0 {
            let port0_base = ahci_base.add(0x100 / 4);
            let port_is_ptr = port0_base.add(PORT_IS / 4);
            let interrupt_status = read_volatile(port_is_ptr);

            write_volatile(port_is_ptr, interrupt_status);

            let port_ci_ptr = port0_base.add(PORT_CI / 4);
            let active_slots = read_volatile(port_ci_ptr);

            // Safely determine completion by comparing software-pending state against hardware PORT_CI
            let pending = PENDING_SLOTS.load(Ordering::Acquire);
            let completed = pending & !active_slots;
            
            if completed != 0 {
                PENDING_SLOTS.fetch_and(!completed, Ordering::Release);
                for slot in 0..32 {
                    if (completed & (1 << slot)) != 0 {
                        SLOT_COMPLETION[slot].store(true, Ordering::Release);
                    }
                }
            }
        }
    }
}
