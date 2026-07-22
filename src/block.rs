//! Block Device Abstraction for mitosOS.

pub const SECTOR_SIZE: usize = 512;

/// Common interface for block storage drivers (VirtIO, NVMe, RAM Disks).
pub trait BlockDevice: Send + Sync {
    /// Reads a single 512-byte sector from the storage device.
    fn read_sector(&self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str>;

    /// Writes a single 512-byte sector to the storage device.
    fn write_sector(&mut self, sector_id: usize, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str>;
}

/// A RAM-backed block device for testing filesystems in memory before attaching VirtIO hardware.
pub struct RamBlockDevice {
    data: alloc::vec::Vec<u8>,
}

impl RamBlockDevice {
    pub fn new(size_in_sectors: usize) -> Self {
        Self {
            data: alloc::vec![0u8; size_in_sectors * SECTOR_SIZE],
        }
    }
}

impl BlockDevice for RamBlockDevice {
    fn read_sector(&self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        let start = sector_id * SECTOR_SIZE;
        let end = start + SECTOR_SIZE;
        if end > self.data.len() {
            return Err("Block read out of bounds");
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_sector(&mut self, sector_id: usize, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        let start = sector_id * SECTOR_SIZE;
        let end = start + SECTOR_SIZE;
        if end > self.data.len() {
            return Err("Block write out of bounds");
        }
        self.data[start..end].copy_from_slice(buf);
        Ok(())
    }
}
