// Repo path: src/fs/mbr.rs
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

const PARTITION_TABLE_OFFSET: usize = 446; // 0x1BE
const PARTITION_ENTRY_SIZE: usize = 16;
const PARTITION_ENTRY_COUNT: usize = 4;
const TYPE_EMPTY: u8 = 0x00;

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
/// normal FAT partitions -- filtering by type risks false negatives
/// for no real safety benefit, since `Fat32FileSystem::mount` already
/// validates the target sector is an actual FAT BPB before trusting
/// it.
pub fn find_first_partition_lba(device: &mut dyn BlockDevice) -> Result<Option<u32>, &'static str> {
    let mut sector0 = [0u8; SECTOR_SIZE];
    device.read_sector(0, &mut sector0)?;

    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Ok(None);
    }

    for i in 0..PARTITION_ENTRY_COUNT {
        let entry_start = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_SIZE;
        let entry = &sector0[entry_start..entry_start + PARTITION_ENTRY_SIZE];

        let partition_type = entry[4];
        if partition_type == TYPE_EMPTY {
            continue;
        }

        let lba_start = u32::from_le_bytes(entry[8..12].try_into().unwrap());
        if lba_start == 0 {
            continue;
        }

        return Ok(Some(lba_start));
    }

    Ok(None)
}
