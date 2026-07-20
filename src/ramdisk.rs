//! Read-only USTAR (tar) ramdisk file system for mitosOS.
//! Zero-allocation implementation designed for bare-metal environments.

use core::str;

// =========================================================================
// 1. USTAR (Tar) Header Definition
// =========================================================================

/// Standard USTAR Header (exactly 512 Bytes)
#[repr(C)]
pub struct UstarHeader {
    pub name: [u8; 100],
    pub mode: [u8; 8],
    pub uid: [u8; 8],
    pub gid: [u8; 8],
    pub size: [u8; 12],
    pub mtime: [u8; 12],
    pub checksum: [u8; 8],
    pub typeflag: u8,
    pub linkname: [u8; 100],
    pub magic: [u8; 6],
    pub version: [u8; 2],
    pub uname: [u8; 32],
    pub gname: [u8; 32],
    pub devmajor: [u8; 8],
    pub devminor: [u8; 8],
    pub prefix: [u8; 155],
    pub pad: [u8; 12],
}

// Compile-time assertion to strictly enforce the 512-byte hardware requirement.
// If anyone accidentally modifies the struct above, the kernel will refuse to compile.
const _: () = assert!(core::mem::size_of::<UstarHeader>() == 512);

impl UstarHeader {
    /// Parses the octal ASCII size field into a standard `usize`.
    pub fn file_size(&self) -> usize {
        let mut size = 0;
        for &byte in self.size.iter() {
            // Tar size strings can be null-terminated or space-terminated.
            if byte == 0 || byte == b' ' {
                break;
            }
            if byte >= b'0' && byte <= b'7' {
                size = (size << 3) | ((byte - b'0') as usize);
            }
        }
        size
    }

    /// Safely extracts the filename as a string slice without allocating memory.
    pub fn filename(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        str::from_utf8(&self.name[..len]).unwrap_or("<invalid utf8>")
    }

    /// Checks the magic bytes to verify this is a structurally valid USTAR block.
    pub fn is_valid(&self) -> bool {
        self.magic == *b"ustar\0" || self.magic == *b"ustar "
    }
}

// =========================================================================
// 2. File Entry Abstraction
// =========================================================================

#[derive(Debug, Clone, Copy)]
pub struct FileEntry<'a> {
    pub name: &'a str,
    pub size: usize,
    pub data: &'a [u8],
    pub typeflag: u8,
}

impl<'a> FileEntry<'a> {
    /// Returns true if the entry is a standard file.
    pub fn is_file(&self) -> bool {
        self.typeflag == 0 || self.typeflag == b'0'
    }

    /// Returns true if the entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.typeflag == b'5'
    }

    /// Attempts to parse the underlying binary data as a UTF-8 string slice.
    pub fn as_text(&self) -> Option<&'a str> {
        str::from_utf8(self.data).ok()
    }
}

// =========================================================================
// 3. Ramdisk File System Engine
// =========================================================================

pub struct TarFileSystem<'a> {
    data: &'a [u8],
}

impl<'a> TarFileSystem<'a> {
    /// Initializes the file system from a raw memory slice.
    ///
    /// # Safety
    /// The caller must ensure the memory range `base_addr` to `base_addr + size` 
    /// is physically mapped, valid, and contains the ramdisk payload.
    pub unsafe fn new(base_addr: usize, size: usize) -> Option<Self> {
        if size < 512 {
            return None; // Too small to even hold one header
        }

        let data = core::slice::from_raw_parts(base_addr as *const u8, size);
        
        // Sanity check the very first block
        let first_header = unsafe { &*(data.as_ptr() as *const UstarHeader) };
        if !first_header.is_valid() {
            return None; 
        }

        Some(Self { data })
    }

    /// Returns an iterator over all files in the archive.
    pub fn files(&self) -> TarIterator<'a> {
        TarIterator {
            data: self.data,
            offset: 0,
        }
    }

    /// Performs an O(n) scan to find a file by an exact name match.
    pub fn find(&self, target_name: &str) -> Option<FileEntry<'a>> {
        self.files().find(|entry| entry.name == target_name)
    }
}

// =========================================================================
// 4. Zero-Allocation Iterator
// =========================================================================

pub struct TarIterator<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for TarIterator<'a> {
    type Item = FileEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.offset + 512 <= self.data.len() {
            let header_ptr = &self.data[self.offset] as *const u8 as *const UstarHeader;
            let header = unsafe { &*header_ptr };

            // Two consecutive zero-blocks mark the end of a tar archive.
            if header.name[0] == 0 {
                return None;
            }

            // If a block is non-zero but invalid, the archive is truncated/corrupted.
            if !header.is_valid() {
                return None; 
            }

            let file_size = header.file_size();
            let data_start = self.offset + 512;
            
            // Tar always pads file payloads out to the nearest 512-byte block boundary.
            let padded_size = (file_size + 511) & !511;

            // Advance the iterator offset for the *next* call.
            self.offset = data_start + padded_size;

            // Bounds check to prevent kernel panics on corrupted file sizes.
            let safe_data_slice = if data_start + file_size <= self.data.len() {
                &self.data[data_start..data_start + file_size]
            } else {
                &[] 
            };

            return Some(FileEntry {
                name: header.filename(),
                size: file_size,
                data: safe_data_slice,
                typeflag: header.typeflag,
            });
        }

        None
    }
}
