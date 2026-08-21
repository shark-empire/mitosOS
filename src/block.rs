//! Block Device Abstraction for mitosOS.

pub const SECTOR_SIZE: usize = 512;

pub trait BlockDevice: Send + Sync {
    fn read_sector(&mut self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str>;
    
    // Put write_sector to work:
    fn write_sector(&mut self, sector_id: usize, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str>;
    
    fn sector_size(&self) -> usize {
        SECTOR_SIZE
    }
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
    fn read_sector(&mut self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
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

/// A `BlockDevice` adapter that offsets every sector access by a fixed
/// LBA, so a filesystem driver written against "sector 0 == start of
/// volume" (like `fs::fat32`) can be mounted on a real MBR-partitioned
/// disk without knowing anything about partitioning itself. The offset
/// normally comes from `fs::mbr::find_first_partition_lba`.
pub struct PartitionBlockDevice {
    inner: alloc::boxed::Box<dyn BlockDevice>,
    lba_offset: usize,
}

impl PartitionBlockDevice {
    pub fn new(inner: alloc::boxed::Box<dyn BlockDevice>, lba_offset: usize) -> Self {
        Self { inner, lba_offset }
    }
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sector(&mut self, sector_id: usize, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        self.inner.read_sector(sector_id + self.lba_offset, buf)
    }

    fn write_sector(&mut self, sector_id: usize, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        self.inner.write_sector(sector_id + self.lba_offset, buf)
    }

    fn sector_size(&self) -> usize {
        self.inner.sector_size()
    }
}
