extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::sync::Arc;
use spin::Mutex;
use super::{FileSystem, FileNode};

pub struct MountPoint {
    path: String,
    fs: Arc<dyn FileSystem>,
}

pub struct VirtualFileSystem {
    mounts: Vec<MountPoint>,
}

impl VirtualFileSystem {
    pub const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    pub fn mount(&mut self, path: &str, fs: Arc<dyn FileSystem>) {
        self.mounts.push(MountPoint {
            path: String::from(path),
            fs,
        });
    }

    // Resolves any path across mounted filesystems
    pub fn open(&self, path: &str) -> Option<Arc<dyn FileNode>> {
        // Simple VFS routing logic: find the longest matching mount prefix
        for mount in &self.mounts {
            if path.starts_with(&mount.path) {
                let relative_path = &path[mount.path.len()..];
                let lookup_path = if relative_path.is_empty() { "/" } else { relative_path };
                return mount.fs.lookup(lookup_path);
            }
        }
        None
    }
}

// Global static VFS instance protected by a spinlock
pub static VFS: Mutex<VirtualFileSystem> = Mutex::new(VirtualFileSystem::new());
