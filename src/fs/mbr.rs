//! Minimal MBR (Master Boot Record) partition table reader.
//!
//! Exists because a block device that presents itself as a *disk*
//! (rather than a bare, unpartitioned "superfloppy" volume) puts a
//! partition table in LBA 0, not a filesystem boot sector -- real hard
//! disks and USB sticks are like this, and so is QEMU's own `fat:rw:dir`
//! vvfat driver when attached `if=ide` (as opposed to `if=floppy`): it
//! synthesizes a classic CHS-geometry MBR with the FAT volume starting
//! at the first full-cylinder boundary (LBA 63 for the default 16
//! heads/63 sectors-per-track geometry -- matches the 1,032,192 = 63 *
//! 16 * 1024 sector count `AtaDevice::new()` reports for this project's
//! CI disk image exactly), not at LBA 0.
//!
//! A filesystem driver that blindly mounts at LBA 0 on such a disk ends
//! up parsing the MBR's own boot code bytes as if they were a FAT BPB.
//! That still passes a "does the 0x55AA boot signature exist" check --
//! every MBR has one too, at the same offset a FAT boot sector does --
//! but everything else is garbage, including `bytes_per_sector`, which
//! is exactly the "Unsupported sector size" failure this module exists
//! to prevent.

use crate::block::{BlockDevice, SECTOR_SIZE};

/// Offset in the MBR sector where the partition table begins.
const PARTITION_TABLE_OFFSET: usize = 0x1BE;

/// Size of a single MBR partition entry in bytes.
const PARTITION_ENTRY_SIZE: usize = 16;

/// The MBR partition table supports exactly 4 primary partitions.
const PARTITION_ENTRY_COUNT: usize = 4;

/// Partition type indicating an empty/unused slot.
const TYPE_EMPTY: u8 = 0x00;

/// Offset of the boot signature at the end of the 512-byte sector.
const BOOT_SIGNATURE_OFFSET: usize = 510;
const BOOT_SIGNATURE_BYTE_0: u8 = 0x55;
const BOOT_SIGNATURE_BYTE_1: u8 = 0xAA;

/// Represents a single 16-byte MBR partition entry.
#[derive(Debug, Clone, Copy)]
pub struct PartitionEntry {
    pub status: u8,
    pub chs_first: [u8; 3],
    pub partition_type: u8,
    pub chs_last: [u8; 3],
    pub lba_start: u32,
    pub sector_count: u32,
}

impl PartitionEntry {
    /// Safely parses a 16-byte slice into a `PartitionEntry`.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            status: bytes[0],
            chs_first: [bytes[1], bytes[2], bytes[3]],
            partition_type: bytes[4],
            chs_last: [bytes[5], bytes[6], bytes[7]],
            // Using unwrap_or provides a safe fallback if slice conversion fails
            lba_start: u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4])),
            sector_count: u32::from_le_bytes(bytes[12..16].try_into().unwrap_or([0; 4])),
        }
    }

    /// Returns true if this partition entry is marked as empty.
    pub fn is_empty(&self) -> bool {
        self.partition_type == TYPE_EMPTY
    }
}

/// Reads LBA 0 and returns the starting LBA of the first non-empty
/// partition table entry, if this looks like a real MBR (0x55AA
/// signature present). Returns `Ok(None)` if LBA 0 is *not* an MBR at
/// all (no boot signature) -- the caller should then try mounting
/// directly at LBA 0, since some volumes (floppies, QEMU's
/// `if=floppy` vvfat mode, a bare `dd`-imaged FAT partition) really do
/// start their filesystem there with no partition table in front of
/// it.
///
/// Deliberately takes the *first* non-empty entry regardless of its
/// partition type byte: QEMU vvfat's synthesized MBR uses whichever
/// type byte matches the FAT flavour it picked (0x0B/0x0C for FAT32,
/// 0x06/0x0E for FAT16, 0x01/0x04 for FAT12/16), and real-world disks
/// occasionally carry OEM-specific type bytes for otherwise perfectly
/// normal FAT partitions.
pub fn find_first_partition_lba(device: &mut dyn BlockDevice) -> Result<Option<u32>, &'static str> {
    let mut sector0 = [0u8; SECTOR_SIZE];
    
    // Read the first sector, propagating any hardware I/O errors up to the caller
    device.read_sector(0, &mut sector0)?;

    // Validate the MBR boot signature to ensure we aren't reading garbage
    if sector0[BOOT_SIGNATURE_OFFSET] != BOOT_SIGNATURE_BYTE_0 
        || sector0[BOOT_SIGNATURE_OFFSET + 1] != BOOT_SIGNATURE_BYTE_1 
    {
        return Ok(None);
    }

    // Iterate through the 4 standard MBR partition entries
    for i in 0..PARTITION_ENTRY_COUNT {
        let start = PARTITION_TABLE_OFFSET + (i * PARTITION_ENTRY_SIZE);
        let end = start + PARTITION_ENTRY_SIZE;
        
        let entry = PartitionEntry::from_bytes(&sector0[start..end]);

        if entry.is_empty() {
            continue;
        }

        // Ignore partitions that claim to start at LBA 0 (which would be recursive)
        if entry.lba_start == 0 {
            continue;
        }

        return Ok(Some(entry.lba_start));
    }

    Ok(None)
}
