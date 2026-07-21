use alloc::string::Stringi;
use alloc::vec::Vec;

pub mod vfs;

pub enum NodeType {
    File,
    Directory,
}

pub struct Metadata {
    pub name: String,
    pub size: usize,
    pub node_type: NodeType,
}

pub trait FileNode {
    fn metadata(&self) -> Metadata;
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str>;
    // Future write support for writeable filesystems like FAT32:
    // fn write(&mut self, offset: usize, buf: &[u8]) -> Result<usize, &'static str>;
}

pub trait FileSystem: Send + Sync {
    fn root(&self) -> alloc::sync::Arc<dyn FileNode>;
    fn lookup(&self, path: &str) -> Option<alloc::sync::Arc<dyn FileNode>>;
}
