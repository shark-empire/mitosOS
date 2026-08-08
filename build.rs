use std::env;
use std::fs;
use std::io::{Write, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let target = env::var("TARGET").unwrap();
    if !target.contains("x86_64") {
        return; // Only x86_64 builds need the custom bootloader
    }

    // 1. Assemble stage1.s
    let stage1_bin = format!("{}/stage1.bin", out_dir);
    let status = Command::new("nasm")
        .args(&["-f", "bin", "bootloader/src/stage1.s", "-o", &stage1_bin])
        .status()
        .expect("nasm not found");
    if !status.success() {
        panic!("Failed to assemble stage1.s");
    }

    // 2. Assemble stage2.s (should produce exactly 32KB)
    let stage2_bin = format!("{}/stage2.bin", out_dir);
    let status = Command::new("nasm")
        .args(&["-f", "bin", "bootloader/src/stage2.s", "-o", &stage2_bin])
        .status()
        .expect("nasm not found");
    if !status.success() {
        panic!("Failed to assemble stage2.s");
    }

    // 3. Build the kernel (Rust) and get the raw binary
    //    We rely on Cargo to produce the ELF, then we objcopy to raw.
    //    The kernel ELF is at target/.../mitosos (or with .elf extension)
    let kernel_elf = env::var("CARGO_BIN_FILE_MITOSOS").unwrap();
    let kernel_bin = format!("{}/kernel.bin", out_dir);
    let status = Command::new("objcopy")
        .args(&["-O", "binary", &kernel_elf, &kernel_bin])
        .status()
        .expect("objcopy not found");
    if !status.success() {
        panic!("Failed to objcopy kernel to binary");
    }

    // 4. (Optional) Build your ramdisk – you need to provide a ramdisk image.
    //    For now we assume a file named "ramdisk.img" exists in the project root.
    //    If not, create a dummy one (128KB of zeros) for testing.
    let ramdisk_src = "ramdisk.img";
    let ramdisk_bin = format!("{}/ramdisk.bin", out_dir);
    if Path::new(ramdisk_src).exists() {
        fs::copy(ramdisk_src, &ramdisk_bin).unwrap();
    } else {
        // Create a dummy ramdisk (128KB of zeros)
        let mut f = fs::File::create(&ramdisk_bin).unwrap();
        f.write_all(&[0; 128 * 1024]).unwrap();
    }

    // 5. Create the final disk.img by concatenating with padding
    let disk_path = format!("{}/disk.img", out_dir);
    let mut disk = fs::File::create(&disk_path).unwrap();

    // Write stage1 (512 bytes)
    let stage1_data = fs::read(&stage1_bin).unwrap();
    if stage1_data.len() != 512 {
        panic!("stage1.bin must be exactly 512 bytes");
    }
    disk.write_all(&stage1_data).unwrap();

    // Write stage2 (exactly 32KB = 64 sectors)
    let stage2_data = fs::read(&stage2_bin).unwrap();
    if stage2_data.len() > 32 * 1024 {
        panic!("stage2.bin exceeds 32KB");
    }
    disk.write_all(&stage2_data).unwrap();
    // Pad to 32KB if shorter
    let pad_len = 32 * 1024 - stage2_data.len();
    if pad_len > 0 {
        disk.write_all(&vec![0; pad_len]).unwrap();
    }

    // Write kernel binary (exactly 384KB = 768 sectors)
    let kernel_data = fs::read(&kernel_bin).unwrap();
    if kernel_data.len() > 384 * 1024 {
        panic!("Kernel binary exceeds 384KB");
    }
    disk.write_all(&kernel_data).unwrap();
    let pad_len = 384 * 1024 - kernel_data.len();
    if pad_len > 0 {
        disk.write_all(&vec![0; pad_len]).unwrap();
    }

    // Write ramdisk binary (exactly 128KB = 256 sectors)
    let ramdisk_data = fs::read(&ramdisk_bin).unwrap();
    if ramdisk_data.len() > 128 * 1024 {
        panic!("Ramdisk exceeds 128KB");
    }
    disk.write_all(&ramdisk_data).unwrap();
    let pad_len = 128 * 1024 - ramdisk_data.len();
    if pad_len > 0 {
        disk.write_all(&vec![0; pad_len]).unwrap();
    }

    // 6. Copy the final disk.img to the project root for CI
    fs::copy(&disk_path, "disk.img").unwrap();

    // Tell Cargo to rerun if any of these files change
    println!("cargo:rerun-if-changed=src/stage1.s");
    println!("cargo:rerun-if-changed=src/stage2.s");
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-changed=ramdisk.img");
}
