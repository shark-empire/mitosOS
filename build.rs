fn main() {
    let target = std::env::var("TARGET").unwrap();

    // Only compile boot.s if we are targeting AArch64
    if target.contains("aarch64") {
        cc::Build::new()
            .target(&target)
            .file("src/boot.s")
            // Explicitly set the assembler flags if needed
            .compiler("aarch64-linux-gnu-gcc") 
            .compile("boot");
    }

    println!("cargo:rerun-if-changed=src/boot.s");
}
