//! Professional Production-Ready FAT32 Filesystem Driver for mitosOS.

use crate::block::{BlockDevice, SECTOR_SIZE};
use alloc::vec::Vec;
use alloc::string::String;
use alloc::boxed::Box;

/// FAT32 Directory Entry Attributes
pub const ATTR_READ_ONLY: u8 = 0x01;
pub const ATTR_HIDDEN: u8    = 0x02;
pub const ATTR_SYSTEM: u8    = 0x04;
pub const ATTR_VOLUME_ID: u8 = 0x08;
pub const ATTR_DIRECTORY: u8 = 0x10;
pub const ATTR_ARCHIVE: u8   = 0x20;
pub const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

/// Parsed geometry and metadata for a FAT32 volume.
#[derive(Debug, Clone, Copy)]
pub struct Fat32Geometry {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sector_count: u16,
    pub num_fats: u8,
    pub sectors_per_fat: u32,
    pub root_cluster: u32,
    pub first_fat_sector: usize,
    pub first_data_sector: usize,
}

/// A standard 32-byte FAT directory entry.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct FatDirectoryEntry {
    pub name: [u8; 11],
    pub attr: u8,
    pub reserved: u8,
    pub creation_time_tenth: u8,
    pub creation_time: u16,
    pub creation_date: u16,
    pub last_access_date: u16,
    pub first_cluster_high: u16,
    pub write_time: u16,
    pub write_date: u16,
    pub first_cluster_low: u16,
    pub file_size: u32,
}

impl FatDirectoryEntry {
    /// Combines high and low cluster indices into a full 32-bit starting cluster number.
    #[inline]
    pub fn starting_cluster(&self) -> u32 {
        ((self.first_cluster_high as u32) << 16) | (self.first_cluster_low as u32)
    }

    /// Checks if the entry is unused / free.
    #[inline]
    pub fn is_free(&self) -> bool {
        self.name[0] == 0x00 || self.name[0] == 0xE5
    }

    /// Checks if the entry marks the end of the directory listing.
    #[inline]
    pub fn is_end(&self) -> bool {
        self.name[0] == 0x00
    }

    /// Checks if the entry is a long file name (LFN) descriptor.
    #[inline]
    pub fn is_lfn(&self) -> bool {
        (self.attr & ATTR_LONG_NAME) == ATTR_LONG_NAME
    }

    /// Formats the 8.3 filename into a standard readable String.
    pub fn get_name(&self) -> String {
        let mut name_part = Vec::new();
        let mut ext_part = Vec::new();

        for i in 0..8 {
            if self.name[i] == b' ' { break; }
            name_part.push(self.name[i]);
        }

        for i in 8..11 {
            if self.name[i] == b' ' { break; }
            ext_part.push(self.name[i]);
        }

        let mut formatted = String::from_utf8_lossy(&name_part).to_string();
        if !ext_part.is_empty() {
            formatted.push('.');
            formatted.push_str(&String::from_utf8_lossy(&ext_part));
        }
        formatted.to_lowercase()
    }
}

/// FAT32 Filesystem Driver Instance.
pub struct Fat32FileSystem {
    device: Box<dyn BlockDevice>,
    geometry: Fat32Geometry,
}

impl Fat32FileSystem {
    /// Parses the boot sector safely without unsafe packed references and initializes the filesystem.
    pub fn mount(mut device: Box<dyn BlockDevice>) -> Result<Self, &'static str> {
        let mut sector_buf = [0u8; SECTOR_SIZE];
        device.read_sector(0, &mut sector_buf)?;

        // Validate boot sector signature (0x55AA at offset 510)
        if sector_buf[510] != 0x55 || sector_buf[511] != 0xAA {
            return Err("Invalid boot sector signature: missing 0x55AA");
        }

        // Safe little-endian parsing of BPB and EBPB fields
        let bytes_per_sector = u16::from_le_bytes(sector_buf[11..13].try_into().unwrap());
        if bytes_per_sector as usize != SECTOR_SIZE {
            return Err("Unsupported sector size: FAT32 sector size must match system sector size");
        }

        let sectors_per_cluster = sector_buf[13];
        let reserved_sector_count = u16::from_le_bytes(sector_buf[14..16].try_into().unwrap());
        let num_fats = sector_buf[16];
        
        // FAT32 specific fields located in the Extended BPB
        let sectors_per_fat = u32::from_le_bytes(sector_buf[36..40].try_into().unwrap());
        let root_cluster = u32::from_le_bytes(sector_buf[44..48].try_into().unwrap());

        if sectors_per_fat == 0 {
            return Err("Invalid FAT32 volume: sectors_per_fat cannot be zero");
        }

        let first_fat_sector = reserved_sector_count as usize;
        let first_data_sector = first_fat_sector + (num_fats as usize * sectors_per_fat as usize);

        let geometry = Fat32Geometry {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sector_count,
            num_fats,
            sectors_per_fat,
            root_cluster,
            first_fat_sector,
            first_data_sector,
        };

        Ok(Self { device, geometry })
    }

    /// Converts a cluster number to its starting absolute sector index.
    #[inline]
    fn cluster_to_sector(&self, cluster: u32) -> usize {
        self.geometry.first_data_sector + ((cluster as usize - 2) * self.geometry.sectors_per_cluster as usize)
    }

    /// Reads the FAT table entry to find the next cluster in a chain.
    pub fn next_cluster(&mut self, cluster: u32) -> Result<u32, &'static str> {
        let fat_offset = cluster * 4;
        let fat_sector = self.geometry.first_fat_sector + (fat_offset as usize / SECTOR_SIZE);
        let ent_offset = fat_offset as usize % SECTOR_SIZE;

        let mut sector_buf = [0u8; SECTOR_SIZE];
        self.device.read_sector(fat_sector, &mut sector_buf)?;

        let next = u32::from_le_bytes(
            sector_buf[ent_offset..ent_offset + 4]
                .try_into()
                .map_err(|_| "Failed to parse FAT table entry")?
        ) & 0x0FFFFFFF; // Mask top 4 bits

        Ok(next)
    }

    /// Reads an entire cluster into a buffer.
    pub fn read_cluster(&mut self, cluster: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let sectors_per_cluster = self.geometry.sectors_per_cluster as usize;
        let cluster_size = sectors_per_cluster * SECTOR_SIZE;

        if buf.len() < cluster_size {
            return Err("Provided buffer is too small for cluster data");
        }

        let start_sector = self.cluster_to_sector(cluster);
        for i in 0..sectors_per_cluster {
            let offset = i * SECTOR_SIZE;
            self.device.read_sector(start_sector + i, &mut buf[offset..offset + SECTOR_SIZE])?;
        }

        Ok(())
    }

    /// Reads a file's complete contents into a byte vector given its starting cluster and file size.
    pub fn read_file_content(&mut self, start_cluster: u32, file_size: u32) -> Result<Vec<u8>, &'static str> {
        let mut content = Vec::with_capacity(file_size as usize);
        let mut current_cluster = start_cluster;
        let cluster_size = self.geometry.sectors_per_cluster as usize * SECTOR_SIZE;
        let mut cluster_buf = vec![0u8; cluster_size];

        let mut bytes_remaining = file_size as usize;

        while current_cluster < 0x0FFFFFF8 && current_cluster >= 2 {
            self.read_cluster(current_cluster, &mut cluster_buf)?;

            let bytes_to_copy = bytes_remaining.min(cluster_size);
            content.extend_from_slice(&cluster_buf[..bytes_to_copy]);

            if bytes_remaining <= cluster_size {
                break;
            }
            bytes_remaining -= cluster_size;

            current_cluster = self.next_cluster(current_cluster)?;
        }

        Ok(content)
    }

    /// Finds a file or directory entry by name within a given directory cluster.
    pub fn find_in_directory(&mut self, dir_cluster: u32, target_name: &str) -> Result<FatDirectoryEntry, &'static str> {
        let cluster_size = self.geometry.sectors_per_cluster as usize * SECTOR_SIZE;
        let mut cluster_buf = vec![0u8; cluster_size];
        let mut current_cluster = dir_cluster;

        let target_lower = target_name.to_lowercase();

        while current_cluster < 0x0FFFFFF8 && current_cluster >= 2 {
            self.read_cluster(current_cluster, &mut cluster_buf)?;

            let entry_size = core::mem::size_of::<FatDirectoryEntry>();
            let num_entries = cluster_size / entry_size;

            for i in 0..num_entries {
                let offset = i * entry_size;
                let entry_ptr = cluster_buf[offset..offset + entry_size].as_ptr() as *const FatDirectoryEntry;
                let entry = unsafe { *entry_ptr };

                if entry.is_end() {
                    break;
                }
                if entry.is_free() || entry.is_lfn() {
                    continue;
                }

                if entry.get_name() == target_lower {
                    return Ok(entry);
                }
            }

            current_cluster = self.next_cluster(current_cluster)?;
        }

        Err("File or directory not found")
    }

    /// Convenience wrapper to search and read a file by path from the root directory.
    pub fn read_file_by_path(&mut self, path: &str) -> Result<Vec<u8>, &'static str> {
        let trimmed_path = path.trim_start_matches('/');
        if trimmed_path.is_empty() {
            return Err("Invalid file path");
        }

        let parts: Vec<&str> = trimmed_path.split('/').collect();
        let mut current_cluster = self.geometry.root_cluster;

        for (i, part) in parts.iter().enumerate() {
            let entry = self.find_in_directory(current_cluster, part)?;
            
            if i == parts.len() - 1 {
                if (entry.attr & ATTR_DIRECTORY) != 0 {
                    return Err("Target path is a directory, not a file");
                }
                return self.read_file_content(entry.starting_cluster(), entry.file_size);
            } else {
                if (entry.attr & ATTR_DIRECTORY) == 0 {
                    return Err("Parent path component is not a directory");
                }
                current_cluster = entry.starting_cluster();
            }
        }

        Err("Path resolution failed")
    }
}
