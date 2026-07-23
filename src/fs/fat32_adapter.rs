// Repo path: src/fs/fat32_adapter.rs  (NEW FILE)
//! FAT32 adapter implementing VFS traits for mitosOS.
//!
//! `Fat32FileSystem`'s read methods take `&mut self` (they use scratch
//! sector buffers), but the `FileSystem`/`FileNode` VFS traits only hand
//! out `&self`. A lock bridges the two -- using `crate::sync::Spinlock`
//! rather than `spin::Mutex` (what `vfs.rs` uses) or `memory::Mutex`,
//! since it's the one that disables interrupts while held, which matters
//! for anything touching disk I/O.

use alloc::string::String;
use alloc::sync::Arc;

use crate::sync::Spinlock;

use super::{FileNode, FileSystem, Metadata, NodeType};
use super::fat32::{Fat32FileSystem, FatDirectoryEntry, ATTR_DIRECTORY};

// =========================================================================
// 1. File System Adapter
// =========================================================================

/// Adapter that mounts a `Fat32FileSystem` into the VFS hierarchy.
pub struct Fat32Adapter {
    inner: Arc<Spinlock<Fat32FileSystem>>,
}

impl Fat32Adapter {
    pub fn new(inner: Fat32FileSystem) -> Self {
        Self { inner: Arc::new(Spinlock::new(inner)) }
    }
}

impl FileSystem for Fat32Adapter {
    fn root(&self) -> Arc<dyn FileNode> {
        Arc::new(Fat32DirectoryNode)
    }

    fn lookup(&self, path: &str) -> Option<Arc<dyn FileNode>> {
        let clean_path = path.trim_start_matches('/');
        if clean_path.is_empty() {
            return Some(self.root());
        }

        let entry = {
            let mut fs = self.inner.lock();
            fs.resolve_path(clean_path).ok()?
        };

        if (entry.attr & ATTR_DIRECTORY) != 0 {
            Some(Arc::new(Fat32DirectoryNode))
        } else {
            Some(Arc::new(Fat32FileNode {
                fs: Arc::clone(&self.inner),
                entry,
            }))
        }
    }
}

// =========================================================================
// 2. Fat32 File Node
// =========================================================================

/// Represents a single file inside the mounted FAT32 volume.
struct Fat32FileNode {
    fs: Arc<Spinlock<Fat32FileSystem>>,
    entry: FatDirectoryEntry,
}

impl FileNode for Fat32FileNode {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: self.entry.get_name(),
            size: self.entry.file_size as usize,
            node_type: NodeType::File,
        }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let file_size = self.entry.file_size as usize;
        if offset >= file_size {
            return Ok(0); // EOF
        }

        // FAT32 has no offset-aware/streaming read yet, so pull the whole
        // file and slice it. Fine for `cat`-sized reads (shell.rs always
        // reads from offset 0 into a size-matched buffer anyway) -- revisit
        // if you start streaming large files a chunk at a time.
        let content = {
            let mut fs = self.fs.lock();
            fs.read_file_content(self.entry.starting_cluster(), self.entry.file_size)?
        };

        let copy_len = core::cmp::min(buf.len(), content.len() - offset);
        buf[..copy_len].copy_from_slice(&content[offset..offset + copy_len]);
        Ok(copy_len)
    }
}

// =========================================================================
// 3. Fat32 Directory Node
// =========================================================================

/// Represents any directory in the mounted volume. Matches
/// `TarDirectoryNode`'s level of support -- no listing yet, since
/// `FileNode` doesn't have a `readdir`-style method defined.
struct Fat32DirectoryNode;

impl FileNode for Fat32DirectoryNode {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: String::from("/"),
            size: 0,
            node_type: NodeType::Directory,
        }
    }

    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, &'static str> {
        Err("Cannot read a directory as a file")
    }
}
