use super::{FileSystem, FileNode, Metadata, NodeType};
use crate::ramdisk::{TarFileSystem, FileEntry};
use alloc::sync::Arc;
use alloc::string::String;

pub struct TarFsAdapter<'a> {
    inner: TarFileSystem<'a>,
}

impl<'a> TarFsAdapter<'a> {
    pub fn new(inner: TarFileSystem<'a>) -> Self {
        Self { inner }
    }
}

// Implement the abstract FileSystem trait for Tar
impl<'a> FileSystem for TarFsAdapter<'a> {
    fn root(&self) -> Arc<dyn FileNode> {
        Arc::new(TarDirectoryNode { inner: self.inner.clone() })
    }

    fn lookup(&self, path: &str) -> Option<Arc<dyn FileNode>> {
        // Strip leading slash for tar matching
        let clean_path = path.trim_start_matches('/');
        if clean_path.is_empty() {
            return Some(self.root());
        }

        let entry = self.inner.find(clean_path)?;
        Some(Arc::new(TarFileNode { entry }))
    }
}

struct TarFileNode<'a> {
    entry: FileEntry<'a>,
}

impl<'a> FileNode for TarFileNode<'a> {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: String::from(self.entry.name),
            size: self.entry.size,
            node_type: NodeType::File,
        }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let data = self.entry.data;
        if offset >= data.len() {
            return Ok(0);
        }
        let copy_len = core::cmp::min(buf.len(), data.len() - offset);
        buf[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
        Ok(copy_len)
    }
}

struct TarDirectoryNode<'a> {
    inner: TarFileSystem<'a>,
}

impl<'a> FileNode for TarDirectoryNode<'a> {
    fn metadata(&self) -> Metadata {
        Metadata {
            name: String::from("/"),
            size: 0,
            node_type: NodeType::Directory,
        }
    }

    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, &'static str> {
        Err("Cannot read directory as file")
    }
}
