fn main() {
    let target = std::env::var("TARGET").unwrap();

    if target.contains("aarch64") {
        cc::Build::new()
            .target(&target)
            .file("src/boot.s")
            // Ensure you have the cross-compiler installed
            .compiler("aarch64-linux-gnu-gcc")
            .compile("boot_arm");

        println!("cargo:rerun-if-changed=src/boot.s");
    } else if target.contains("x86_64") {
        // boot_x86.s is NASM syntax, like every other .s file in this repo
        // (stage1.s, stage2.s, test_program.s) -- not GAS syntax. cc::Build
        // shells out to the system `cc`, which assembles with GNU `as`: the
        // wrong assembler for this file. GAS treats NASM's `;` comments as
        // statement separators rather than comments, so every comment line
        // gets parsed as a bogus instruction, and none of NASM's directives
        // (section/global/extern/resb) exist in GAS regardless. Assemble
        // with nasm directly instead, matching how build.sh treats every
        // other .s file here, then archive + emit the link directives by
        // hand since we're bypassing cc::Build's own compile-and-link step.
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let obj_path = format!("{}/boot_x86.o", out_dir);
        let status = std::process::Command::new("nasm")
            .args(&["-f", "elf64", "src/boot_x86.s", "-o", &obj_path])
            .status()
            .expect("nasm not found");
        if !status.success() {
            panic!("Failed to assemble src/boot_x86.s");
        }

        let lib_path = format!("{}/libboot_x86.a", out_dir);
        let status = std::process::Command::new("ar")
            .args(&["crus", &lib_path, &obj_path])
            .status()
            .expect("ar not found");
        if !status.success() {
            panic!("Failed to archive boot_x86.o");
        }

        println!("cargo:rustc-link-search=native={}", out_dir);
        println!("cargo:rustc-link-lib=static=boot_x86");
        println!("cargo:rerun-if-changed=src/boot_x86.s");
    }
}
