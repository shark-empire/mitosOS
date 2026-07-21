//! System version and identity metadata for mitosOS.

/// OS Kernel Name
pub const OS_NAME: &str = "mitosOS";

/// Kernel Version
pub const OS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Node/Host Name
pub const OS_NODENAME: &str = "mitos";

/// Target Architecture String
pub const OS_ARCH: &str = if cfg!(target_arch = "aarch64") {
    "aarch64"
} else if cfg!(target_arch = "x86_64") {
    "x86_64"
} else {
    "unknown"
};

/// Structure representing system identity (POSIX `utsname` standard).
#[repr(C)]
pub struct UtsName {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

impl UtsName {
    /// Creates a zeroed `UtsName` instance.
    pub const fn new() -> Self {
        Self {
            sysname: [0; 65],
            nodename: [0; 65],
            release: [0; 65],
            version: [0; 65],
            machine: [0; 65],
            domainname: [0; 65],
        }
    }

    /// Populates the structure fields with current OS constants.
    pub fn populate(&mut self) {
        copy_str(&mut self.sysname, OS_NAME);
        copy_str(&mut self.nodename, OS_NODENAME);
        copy_str(&mut self.release, OS_VERSION);
        copy_str(&mut self.version, OS_VERSION);
        copy_str(&mut self.machine, OS_ARCH);
        copy_str(&mut self.domainname, "(none)");
    }
}

/// Helper to safely copy a string into a null-terminated byte buffer.
fn copy_str(dst: &mut [u8], src: &str) {
    let bytes = src.as_bytes();
    let len = bytes.len().min(dst.len() - 1);
    dst[..len].copy_from_slice(&bytes[..len]);
    dst[len] = 0; // Ensure null termination
}
