#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

STAGE2_MAX_BYTES=32768      # 64 sectors × 512 — must match STAGE2_SECTOR_COUNT in stage1.s
KERNEL_MAX_BYTES=131072     # 256 sectors × 512 — must match KERNEL_SECTOR_COUNT in stage2.s
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

echo "==> Building disk image (stage1 + stage2 + kernel)"
cat stage1.bin stage2.bin kernel.bin > disk.img

echo "==> Done: disk.img ready"
echo "    Test with: qemu-system-x86_64 -drive format=raw,file=disk.img"
