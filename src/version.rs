
pub const OS_NAME: &str = "mitosOS";
pub const OS_VERSION: &str = "0.1.0";
pub const OS_ARCH: &str = if cfg!(target_arch = "aarch64") {
    "AArch64"
} else {
    "x86_64"
};
