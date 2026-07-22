#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

STAGE2_MAX_BYTES=32768      # 64 sectors × 512 — must match STAGE2_SECTOR_COUNT in stage1.s
KERNEL_MAX_BYTES=131072     # 256 sectors × 512 — must match KERNEL_SECTOR_COUNT in stage2.s
RAMDISK_MAX_BYTES=131072    # 256 sectors × 512 — must match RAMDISK_TOTAL_SECTORS in stage2.s
KERNEL_TARGET=x86_64-unknown-none

echo "==> Assembling stage1 (flat binary, must be exactly 512 bytes)"
nasm -f bin bootloader/src/stage1.s -o stage1.bin
STAGE1_SIZE=$(stat -c%s stage1.bin 2>/dev/null || stat -f%z stage1.bin)
if [ "$STAGE1_SIZE" -ne 512 ]; then
    echo "ERROR: stage1.bin is $STAGE1_SIZE bytes, must be exactly 512" >&2
    exit 1
fi

echo "==> Assembling stage2 (flat binary, org 0x8000)"
nasm -f bin bootloader/src/stage2.s -o stage2.bin
STAGE2_SIZE=$(stat -c%s stage2.bin 2>/dev/null || stat -f%z stage2.bin)
if [ "$STAGE2_SIZE" -gt "$STAGE2_MAX_BYTES" ]; then
    echo "ERROR: stage2.bin is $STAGE2_SIZE bytes, exceeds ${STAGE2_MAX_BYTES}-byte budget" >&2
    exit 1
fi
truncate -s "$STAGE2_MAX_BYTES" stage2.bin

# =========================================================================
# Ramdisk contents: assembled BEFORE building the kernel, since aarch64's
# include_bytes!("../rootfs.tar") needs the file to exist at compile time.
# =========================================================================
echo "==> Assembling userspace test_program (static ELF64, no libc)"
nasm -f elf64 userspace/test_program.s -o test_program.o
ld -e _start -o test_program test_program.o
rm -f test_program.o

echo "==> Creating Ramdisk (rootfs.tar)"
rm -rf rootfs
mkdir -p rootfs/bin
echo "Hello from mitosOS in-memory filesystem!" > rootfs/test.txt
cp test_program rootfs/bin/test_program
# List paths explicitly (not `-C rootfs .`) so entries are named "bin/test_program"
# and "test.txt" with no leading "./" -- the VFS lookup only strips a leading "/".
tar -cf rootfs.tar -C rootfs bin/test_program test.txt
# Strictly pad the tarball to 128KB so stage2.s doesn't over-read and crash
truncate -s "$RAMDISK_MAX_BYTES" rootfs.tar

echo "==> Building kernel ($KERNEL_TARGET)"
cargo build --release --target "$KERNEL_TARGET"

KERNEL_BIN=$(find "target/$KERNEL_TARGET/release" -maxdepth 1 -type f -executable ! -name "*.d" | head -n1)
if [ -z "$KERNEL_BIN" ]; then
    echo "ERROR: couldn't find built kernel binary in target/$KERNEL_TARGET/release" >&2
    exit 1
fi
cp "$KERNEL_BIN" kernel.bin

KERNEL_SIZE=$(stat -c%s kernel.bin 2>/dev/null || stat -f%z kernel.bin)
if [ "$KERNEL_SIZE" -gt "$KERNEL_MAX_BYTES" ]; then
    echo "ERROR: kernel.bin is $KERNEL_SIZE bytes, exceeds ${KERNEL_MAX_BYTES}-byte budget" >&2
    echo "       bump KERNEL_SECTOR_COUNT in stage2.s if intentional" >&2
    exit 1
fi
truncate -s "$KERNEL_MAX_BYTES" kernel.bin

echo "==> Building disk image (stage1 + stage2 + kernel + ramdisk)"
# Because we strictly padded everything, concatenating them places rootfs.tar exactly at LBA 321!
cat stage1.bin stage2.bin kernel.bin rootfs.tar > disk.img

echo "==> Done: disk.img ready"
echo "    Test with: qemu-system-x86_64 -drive format=raw,file=disk.img"
