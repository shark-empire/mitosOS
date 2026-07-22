//! Virtual file system layer for mitosOS.
//! Defines the core node/metadata abstractions that concrete file systems
//! (ramdisk, FAT32, etc.) implement against.

use alloc::string::String;
use alloc::sync::Arc;

pub mod vfs;
pub mod tar_adapter;
#[cfg(target_arch = "x86_64")]
pub mod ata;
pub mod fat32;
/// The kind of entry a `FileNode` represents.
pub enum NodeType {
    File,
    Directory,
}

/// Basic descriptive information about a file or directory.
pub struct Metadata {
    pub name: String,
    pub size: usize,
    pub node_type: NodeType,
}

/// A single node (file or directory) within a mounted file system.
pub trait FileNode {
    fn metadata(&self) -> Metadata;
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str>;
    // Future write support for writeable filesystems like FAT32:
    // fn write(&mut self, offset: usize, buf: &[u8]) -> Result<usize, &'static str>;
}

/// A mountable file system implementation.
pub trait FileSystem: Send + Sync {
    fn root(&self) -> Arc<dyn FileNode>;
    fn lookup(&self, path: &str) -> Option<Arc<dyn FileNode>>;
}
