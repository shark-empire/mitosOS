use std::env;
use std::process::Command;

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    if target.contains("x86_64") {
        println!("cargo:rerun-if-changed=src/boot_x86.s");
        println!("cargo:rerun-if-changed=src/boot_multiboot2.s");

        // 1. Assemble with NASM
        let status1 = Command::new("nasm")
            .args(["-f", "elf64", "src/boot_x86.s", "-o", &format!("{out_dir}/boot_x86.o")])
            .status()
            .expect("nasm failed to execute");
        let status2 = Command::new("nasm")
            .args(["-f", "elf64", "src/boot_multiboot2.s", "-o", &format!("{out_dir}/boot_multiboot2.o")])
            .status()
            .expect("nasm failed to execute");

        if !status1.success() || !status2.success() {
            panic!("NASM assembly failed");
        }

        // 2. Package into a static archive (libboot_asm.a)
        let status_ar = Command::new("ar")
            .args([
                "crs",
                &format!("{out_dir}/libboot_asm.a"),
                &format!("{out_dir}/boot_x86.o"),
                &format!("{out_dir}/boot_multiboot2.o"),
            ])
            .status()
            .expect("ar failed to execute");

        if !status_ar.success() {
            panic!("ar archiving failed");
        }

        // 3. Force the linker to include ALL symbols from the archive
        println!("cargo:rustc-link-search=native={out_dir}");
        println!("cargo:rustc-link-lib=static:+whole-archive=boot_asm");

    } else if target.contains("aarch64") {
        cc::Build::new()
            .target(&target)
            .file("src/boot.s")
            .compiler("aarch64-linux-gnu-gcc")
            .compile("boot_arm");
        println!("cargo:rerun-if-changed=src/boot.s");
    }
}
