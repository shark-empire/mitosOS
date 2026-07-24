//! ATA PIO Mode Storage Driver

use crate::block::{BlockDevice, SECTOR_SIZE};

// --- ATA Ports ---
const DATA: u16 = 0x1F0;
const ERROR: u16 = 0x1F1;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LOW: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HIGH: u16 = 0x1F5;
const DRIVE_HEAD: u16 = 0x1F6;
const COMMAND: u16 = 0x1F7;
const STATUS: u16 = 0x1F7;
const ALT_STATUS: u16 = 0x3F6;

// --- ATA Commands ---
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY: u8 = 0xEC;

// --- Status Bits ---
const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_DF: u8 = 1 << 5; // Drive Fault
const STATUS_RDY: u8 = 1 << 6;
const STATUS_BSY: u8 = 1 << 7;

const POLL_ATTEMPTS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtaError {
    Timeout,
    DeviceError(u8),
    NoDevice,
}

// ==========================================
// Low-Level Port I/O
// ==========================================

unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
}

unsafe fn outw(port: u16, value: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) };
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags)) };
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe { core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags)) };
    value
}

fn io_delay() {
    for _ in 0..4 { unsafe { inb(ALT_STATUS) }; }
}

// ==========================================
// Device Synchronization
// ==========================================

fn wait_while_busy() -> Result<(), AtaError> {
    for _ in 0..POLL_ATTEMPTS {
        if unsafe { inb(STATUS) } & STATUS_BSY == 0 { return Ok(()); }
    }
    Err(AtaError::Timeout)
}

fn wait_for_data() -> Result<(), AtaError> {
    for _ in 0..POLL_ATTEMPTS {
        let status = unsafe { inb(STATUS) };
        if status & (STATUS_ERR | STATUS_DF) != 0 {
            return Err(AtaError::DeviceError(unsafe { inb(ERROR) }));
        }
        if status & STATUS_BSY == 0 && status & STATUS_DRQ != 0 {
            return Ok(());
        }
    }
    Err(AtaError::Timeout)
}

/// Reads `count` consecutive sectors starting at `lba` into `buf`.
/// Provided for direct shell/debug access.
pub fn read_sectors(lba: u32, count: u32, buf: &mut [u8]) -> Result<(), AtaError> {
    assert_eq!(
        buf.len(),
        count as usize * SECTOR_SIZE,
        "buffer size must match sector count"
    );

    for i in 0..count {
        let current_lba = lba + i;
        wait_while_busy()?;

        unsafe {
            outb(DRIVE_HEAD, 0xE0 | ((current_lba >> 24) as u8 & 0x0F));
            io_delay();

            outb(SECTOR_COUNT, 1);
            outb(LBA_LOW, current_lba as u8);
            outb(LBA_MID, (current_lba >> 8) as u8);
            outb(LBA_HIGH, (current_lba >> 16) as u8);
            outb(COMMAND, CMD_READ_SECTORS);
        }

        wait_for_data()?;

        let start = i as usize * SECTOR_SIZE;
        for chunk in buf[start..start + SECTOR_SIZE].chunks_exact_mut(2) {
            let word = unsafe { inw(DATA) };
            chunk[0] = word as u8;
            chunk[1] = (word >> 8) as u8;
        }
    }

    Ok(())
}

// ==========================================
// The ATA Device
// ==========================================

/// Represents the Primary Master ATA drive.
#[derive(Debug)]
pub struct AtaDevice {
    /// Total addressable sectors (discovered via IDENTIFY)
    pub total_sectors: u32,
}

impl AtaDevice {
    /// Probes the ATA bus, identifies the drive, and initializes it.
    pub fn new() -> Result<Self, &'static str> {
        unsafe {
            // Select Primary Master (0xA0)
            outb(DRIVE_HEAD, 0xA0);
            io_delay();
            
            // Zero out LBA ports before IDENTIFY
            outb(SECTOR_COUNT, 0);
            outb(LBA_LOW, 0);
            outb(LBA_MID, 0);
            outb(LBA_HIGH, 0);
            
            // Send IDENTIFY command
            outb(COMMAND, CMD_IDENTIFY);
            
            // Check if bus is floating (no drive attached)
            if inb(STATUS) == 0 {
                return Err("No ATA drive attached to Primary Master");
            }
        }

        wait_while_busy().map_err(|_| "ATA Drive hung during IDENTIFY")?;
        
        // If LBA_MID or LBA_HIGH are not 0, this is not an ATA drive (it might be ATAPI/CD-ROM)
        unsafe {
            if inb(LBA_MID) != 0 || inb(LBA_HIGH) != 0 {
                return Err("Device is ATAPI (CD-ROM), not an ATA hard drive");
            }
        }

        wait_for_data().map_err(|_| "ATA Drive rejected IDENTIFY command")?;

        // Read the 256-word IDENTIFY payload
        let mut identify_data = [0u16; 256];
        for word in identify_data.iter_mut() {
            *word = unsafe { inw(DATA) };
        }

        // Word 60 and 61 contain the total number of LBA28 addressable sectors
        let total_sectors = (identify_data[60] as u32) | ((identify_data[61] as u32) << 16);

        Ok(Self { total_sectors })
    }

    /// Flushes the disk hardware cache
    pub fn flush_cache(&self) -> Result<(), AtaError> {
        wait_while_busy()?;
        unsafe {
            outb(COMMAND, CMD_CACHE_FLUSH);
        }
        wait_while_busy()?;
        Ok(())
    }
}

// ==========================================
// BlockDevice Trait Implementation
// ==========================================

impl BlockDevice for AtaDevice {
    fn read_sector(&mut self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        let lba = sector_id as u32;
        if lba >= self.total_sectors {
            return Err("ATA Read Out of Bounds");
        }

        wait_while_busy().map_err(|_| "ATA Timeout")?;

        unsafe {
            outb(DRIVE_HEAD, 0xE0 | ((lba >> 24) as u8 & 0x0F));
            io_delay();

            outb(SECTOR_COUNT, 1);
            outb(LBA_LOW, lba as u8);
            outb(LBA_MID, (lba >> 8) as u8);
            outb(LBA_HIGH, (lba >> 16) as u8);
            outb(COMMAND, CMD_READ_SECTORS);
        }

        wait_for_data().map_err(|_| "ATA Device Error during Read")?;

        for chunk in buf.chunks_exact_mut(2) {
            let word = unsafe { inw(DATA) };
            chunk[0] = word as u8;
            chunk[1] = (word >> 8) as u8;
        }

        Ok(())
    }

    fn write_sector(&mut self, sector_id: usize, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        let lba = sector_id as u32;
        if lba >= self.total_sectors {
            return Err("ATA Write Out of Bounds");
        }

        wait_while_busy().map_err(|_| "ATA Timeout")?;

        unsafe {
            outb(DRIVE_HEAD, 0xE0 | ((lba >> 24) as u8 & 0x0F));
            io_delay();

            outb(SECTOR_COUNT, 1);
            outb(LBA_LOW, lba as u8);
            outb(LBA_MID, (lba >> 8) as u8);
            outb(LBA_HIGH, (lba >> 16) as u8);
            outb(COMMAND, CMD_WRITE_SECTORS);
        }

        wait_for_data().map_err(|_| "ATA Device Error during Write")?;

        for chunk in buf.chunks_exact(2) {
            let word = (chunk[0] as u16) | ((chunk[1] as u16) << 8);
            unsafe { outw(DATA, word); }
        }

        self.flush_cache().map_err(|_| "ATA Cache Flush Failed")?;

        Ok(())
    }
}
