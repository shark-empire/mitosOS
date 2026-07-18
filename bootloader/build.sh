#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

STAGE2_MAX_BYTES=32768   # 64 sectors × 512 — must match STAGE2_SECTOR_COUNT in stage1.s

echo "==> Assembling stage1 (flat binary, must be exactly 512 bytes)"
nasm -f bin src/stage1.s -o stage1.bin
STAGE1_SIZE=$(stat -c%s stage1.bin 2>/dev/null || stat -f%z stage1.bin)
if [ "$STAGE1_SIZE" -ne 512 ]; then
    echo "ERROR: stage1.bin is $STAGE1_SIZE bytes, must be exactly 512" >&2
    exit 1
fi

echo "==> Assembling stage2 (ELF object)"
nasm -f elf32 src/stage2.s -o stage2.o

echo "==> Building Rust kernel"
cargo build --release

echo "==> Linking stage2 + kernel into flat binary"
ld -m elf_i386 -T linker.ld -o stage2.bin \
    stage2.o \
    target/i686-mitos/release/libmitos_bootloader.a

STAGE2_SIZE=$(stat -c%s stage2.bin 2>/dev/null || stat -f%z stage2.bin)
if [ "$STAGE2_SIZE" -gt "$STAGE2_MAX_BYTES" ]; then
    echo "ERROR: stage2.bin is $STAGE2_SIZE bytes, exceeds ${STAGE2_MAX_BYTES}-byte budget" >&2
    echo "       bump STAGE2_SECTOR_COUNT in stage1.s and STAGE2_MAX_BYTES here if intentional" >&2
    exit 1
fi

echo "==> Padding stage2 to fixed sector count"
truncate -s "$STAGE2_MAX_BYTES" stage2.bin

echo "==> Building disk image"
cat stage1.bin stage2.bin > disk.img

echo "==> Done: disk.img ready"
echo "    Test with: qemu-system-i386 -drive format=raw,file=disk.img"
