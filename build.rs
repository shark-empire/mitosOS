// build.rs -- assembles the kernel's own entry-point code and tells
// the linker to include it. A build script, not `global_asm!`,
// specifically because of what each target's entry file is written
// in:
//
// - x86_64 (src/boot_x86.s, src/boot_multiboot2.s): NASM syntax
//   (Intel-style `section`/`global`/`equ`/`[rel ...]` addressing,
//   matching every other .s file in this repo -- see build.sh's own
//   direct `nasm -f elf64` use for userspace/test_program.s).
//   `global_asm!` only accepts what LLVM's *integrated* assembler
//   understands, which is not NASM syntax, so this shells out to the
//   real `nasm` directly rather than going through the `cc` crate
//   (whose default compiler expects GAS/AT&T-syntax input and cannot
//   parse these files).
// - aarch64 (src/boot.s): GAS/AArch64 syntax, compiled via a
//   cross-compiler through the `cc` crate, which is the right tool
//   for *that* input.
use std::env;
use std::process::Command;

fn assemble_nasm(src: &str, out_obj: &str) {
    println!("cargo:rerun-if-changed={src}");
    let status = Command::new("nasm")
        .args(["-f", "elf64", src, "-o", out_obj])
        .status()
        .unwrap_or_else(|e| panic!("failed to run nasm on {src}: {e} -- is nasm installed and on PATH?"));
    if !status.success() {
        panic!("nasm failed to assemble {src}");
    }
    println!("cargo:rustc-link-arg={out_obj}");
}

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    if target.contains("x86_64") {
        assemble_nasm("src/boot_x86.s", &format!("{out_dir}/boot_x86.o"));
        assemble_nasm("src/boot_multiboot2.s", &format!("{out_dir}/boot_multiboot2.o"));
    } else if target.contains("aarch64") {
        cc::Build::new()
            .target(&target)
            .file("src/boot.s")
            // Ensure you have the cross-compiler installed.
            .compiler("aarch64-linux-gnu-gcc")
            .compile("boot_arm");
        println!("cargo:rerun-if-changed=src/boot.s");
    }
}
