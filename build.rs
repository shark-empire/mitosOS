fn main() {
    cc::Build::new()
        .file("src/boot.s")
        .compile("boot");
    println!("cargo:rerun-if-changed=src/boot.s");
}
