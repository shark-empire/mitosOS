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
        cc::Build::new()
            .target(&target)
            .file("src/boot_x86.s")
            // For x86_64, the system compiler (gcc/clang) usually works 
            // if you are natively targeting x86_64.
            .compile("boot_x86");
            
        println!("cargo:rerun-if-changed=src/boot_x86.s");
    }
}
