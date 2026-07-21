//! TAR Ramdisk adapter implementing VFS traits for mitosOS.

use alloc::string::String;
use alloc::sync::Arc;

use super::{FileNode, FileSystem, Metadata, NodeType};
use crate::ramdisk::{FileEntry, TarFileSystem};

// =========================================================================
// 1. File System Adapter
// =========================================================================

/// Adapter that mounts a `TarFileSystem` into the VFS hierarchy.
pub struct TarFsAdapter {
    inner: TarFileSystem,
}

impl TarFsAdapter {
    pub fn new(inner: TarFileSystem) -> Self {
        Self { inner }
    }
}

impl FileSystem for TarFsAdapter {
    fn root(&self) -> Arc<dyn FileNode> {
        Arc::new(TarDirectoryNode { inner: self.inner.clone() })
    }

    fn lookup(&self, path: &str) -> Option<Arc<dyn FileNode>> {
        // Strip leading slashes so we can match raw TAR header names
        let clean_path = path.trim_start_matches('/');
        
        if clean_path.is_empty() {
            return Some(self.root());
        }

        let entry = self.inner.find(clean_path)?;
        Some(Arc::new(TarFileNode { entry }))
    }
}

// =========================================================================
// 2. Tar File Node (Handles Files & Subdirectories)
// =========================================================================

/// Represents an individual entry (file or folder) inside the TAR archive.
struct TarFileNode {
    entry: FileEntry,
}

impl FileNode for TarFileNode {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: String::from(self.entry.name),
            size: self.entry.size,
            // Dynamically assign node type based on the TAR header flag
            node_type: if self.entry.is_dir() {
                NodeType::Directory
            } else {
                NodeType::File
            },
        }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        // Prevent accidental reads on directories
        if self.entry.is_dir() {
            return Err("Cannot read directory as a file");
        }

        let data = self.entry.data;
        if offset >= data.len() {
            return Ok(0); // EOF
        }
        
        let copy_len = core::cmp::min(buf.len(), data.len() - offset);
        buf[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
        Ok(copy_len)
    }
}

// =========================================================================
// 3. Tar Root Directory Node
// =========================================================================

/// Represents the root `/` of the mounted TAR archive.
struct TarDirectoryNode {
    #[allow(dead_code)]
    inner: TarFileSystem,
}

impl FileNode for TarDirectoryNode {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: String::from("/"),
            size: 0,
            node_type: NodeType::Directory,
        }
    }

    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, &'static str> {
        Err("Cannot read root directory as a file")
    }
}
