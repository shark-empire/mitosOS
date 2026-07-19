use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap();

    if target.contains("aarch64") {
        cc::Build::new()
            .target(&target)
            .file("src/boot.s")
            .compiler("aarch64-linux-gnu-gcc")
            .compile("boot_arm");
        println!("cargo:rerun-if-changed=src/boot.s");

    } else if target.contains("x86_64") {
        // boot_x86.s is NASM syntax — gcc's assembler (GAS) can't read it.
        // Shell out to nasm directly instead of routing through cc::Build.
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let obj_path = format!("{out_dir}/boot_x86.o");

        let status = Command::new("nasm")
            .args(["-f", "elf64", "src/boot_x86.s", "-o", &obj_path])
            .status()
            .expect("nasm not found — install it and ensure it's on PATH");
        if !status.success() {
            panic!("nasm failed to assemble src/boot_x86.s");
        }

        cc::Build::new().object(&obj_path).compile("boot_x86");
        println!("cargo:rerun-if-changed=src/boot_x86.s");
    }
}
