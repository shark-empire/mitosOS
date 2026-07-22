//! Minimal FAT32 Filesystem Sector Parser for mitosOS.

use crate::block::{BlockDevice, SECTOR_SIZE};

#[repr(C, packed)]
pub struct Fat32BootSector {
    pub jump_boot: [u8; 3],
    pub oem_name: [u8; 8],
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sector_count: u16,
    pub num_fats: u8,
    pub root_entry_count: u16,
    pub total_sectors_16: u16,
    pub media: u8,
    pub fat_size_16: u16,
    pub sectors_per_track: u16,
    pub number_of_heads: u16,
    pub hidden_sectors: u32,
    pub total_sectors_32: u32,
}

pub fn parse_fat32_boot_sector(device: &dyn BlockDevice) -> Result<usize, &'static str> {
    let mut sector_buf = [0u8; SECTOR_SIZE];
    device.read_sector(0, &mut sector_buf)?;

    let bpb = unsafe { &*(sector_buf.as_ptr() as *const Fat32BootSector) };
    if bpb.bytes_per_sector as usize != SECTOR_SIZE {
        return Err("Invalid sector size for FAT32");
    }

    let reserved = bpb.reserved_sector_count as usize;
    Ok(reserved) // Returns start of FAT tables
}
