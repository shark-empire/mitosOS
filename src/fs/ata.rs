//! Minimal, read-only ATA PIO (Programmed I/O) driver for the
//! primary IDE bus, LBA28 addressing. x86_64 only — this legacy
//! ISA-compatibility interface has no aarch64/Pi equivalent, which
//! would need its own SD/EMMC driver as a separate piece.
//!
//! Targets the primary master drive specifically, matching how
//! `-M pc` (QEMU's PIIX3 IDE controller) attaches `disk.img` as
//! the sole drive in this kernel's boot setup.
//!
//! Read-only by design — nothing built on this (FAT32 included)
//! needs to write.
//!
//! Known gap: this assumes a drive is present and responsive rather
//! than confirming it via an IDENTIFY DEVICE command first. A
//! missing drive will time out after `POLL_ATTEMPTS` iterations
//! rather than being detected upfront — acceptable for now since
//! this kernel's only disk is the one it booted from, but worth
//! revisiting if that assumption ever stops holding.

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

const CMD_READ_SECTORS: u8 = 0x20;

const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_BSY: u8 = 1 << 7;

/// Bound on status-register polling. Real/emulated drives respond in
/// a handful of iterations; this exists to bound a hang if a drive
/// is missing or misbehaving, not because normal reads approach it.
const POLL_ATTEMPTS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtaError {
    /// The controller never cleared BSY / set DRQ within
    /// `POLL_ATTEMPTS` iterations — likely no drive attached.
    Timeout,
    /// The drive set ERR; the value is the raw ATA error register
    /// (0x1F1), whose bits identify the specific fault.
    DeviceError(u8),
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al", in("dx") port, in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx", out("al") value, in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!(
            "in ax, dx", out("ax") value, in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Reading the alternate status register discards its result but
/// costs real time per access — four reads is the standard idiom
/// for the ~400ns settle time a drive needs after a drive-select
/// write, before its status can be trusted.
fn io_delay() {
    for _ in 0..4 {
        unsafe { inb(ALT_STATUS) };
    }
}

fn wait_while_busy() -> Result<(), AtaError> {
    for _ in 0..POLL_ATTEMPTS {
        if unsafe { inb(STATUS) } & STATUS_BSY == 0 {
            return Ok(());
        }
    }
    Err(AtaError::Timeout)
}

fn wait_for_data() -> Result<(), AtaError> {
    for _ in 0..POLL_ATTEMPTS {
        let status = unsafe { inb(STATUS) };
        if status & STATUS_ERR != 0 {
            return Err(AtaError::DeviceError(unsafe { inb(ERROR) }));
        }
        if status & STATUS_BSY == 0 && status & STATUS_DRQ != 0 {
            return Ok(());
        }
    }
    Err(AtaError::Timeout)
}

/// Reads one 512-byte sector at `lba` (28-bit) from the primary
/// master drive into `buf`.
pub fn read_sector(lba: u32, buf: &mut [u8; 512]) -> Result<(), AtaError> {
    assert!(lba < (1 << 28), "LBA28 only supports addresses below 2^28");

    wait_while_busy()?;

    unsafe {
        // 0xE0: LBA mode + drive 0 (master) + LBA bits 27:24
        outb(DRIVE_HEAD, 0xE0 | ((lba >> 24) as u8 & 0x0F));
        io_delay();

        outb(SECTOR_COUNT, 1);
        outb(LBA_LOW, lba as u8);
        outb(LBA_MID, (lba >> 8) as u8);
        outb(LBA_HIGH, (lba >> 16) as u8);
        outb(COMMAND, CMD_READ_SECTORS);
    }

    wait_for_data()?;

    // Each 16-bit word from the data port packs two sector bytes in
    // the same low/high order x86 already uses natively, so this
    // reassembly needs no explicit endianness handling beyond that.
    for chunk in buf.chunks_exact_mut(2) {
        let word = unsafe { inw(DATA) };
        chunk[0] = word as u8;
        chunk[1] = (word >> 8) as u8;
    }

    Ok(())
}

/// Reads `count` consecutive sectors starting at `lba` into `buf`,
/// which must be exactly `count * 512` bytes. Will be how FAT32
/// cluster reads work, since a cluster is normally several sectors.
pub fn read_sectors(lba: u32, count: u32, buf: &mut [u8]) -> Result<(), AtaError> {
    assert_eq!(
        buf.len(),
        count as usize * 512,
        "buffer size must match sector count"
    );

    for i in 0..count {
        let mut sector = [0u8; 512];
        read_sector(lba + i, &mut sector)?;
        let start = i as usize * 512;
        buf[start..start + 512].copy_from_slice(&sector);
    }

    Ok(())
}

/// Thin wrapper giving callers a `Self` to hold and pass around instead of
/// calling the free functions above directly. Doesn't add new behavior --
/// exists so future disk-backed code (e.g. a filesystem driver) has a
/// value to construct rather than a bag of module functions.
#[derive(Debug, Clone, Copy)]
pub struct AtaDevice;

impl AtaDevice {
    pub const fn new() -> Self {
        Self
    }

    pub fn read_sector(&self, lba: u32, buf: &mut [u8; 512]) -> Result<(), AtaError> {
        read_sector(lba, buf)
    }

    pub fn read_sectors(&self, lba: u32, count: u32, buf: &mut [u8]) -> Result<(), AtaError> {
        read_sectors(lba, count, buf)
    }
}

