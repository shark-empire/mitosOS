#!/bin/bash
set -e

echo "========================================"
echo " 1. Compiling mitosOS Ring 3 Userspace"
echo "========================================"
cd userspace
cargo build --release
cd ..

echo "========================================"
echo " 2. Injecting ELF Binaries into Kernel"
echo "========================================"
mkdir -p user_binaries
# The workspace build outputs to the workspace target directory
cp userspace/target/x86_64-unknown-none/release/test_app user_binaries/test_app.elf
echo "Success: test_app.elf copied to user_binaries/"

echo "========================================"
echo " 3. Booting mitosOS"
echo "========================================"
# Run the kernel (assuming you use bootimage/QEMU via cargo run)
cargo run
