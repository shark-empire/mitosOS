#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

STAGE2_MAX_BYTES=32768      # 64 sectors × 512 — must match STAGE2_SECTOR_COUNT in stage1.s
KERNEL_MAX_BYTES=393216    # 768 sectors × 512 — must match KERNEL_TOTAL_SECTORS in stage2.s
RAMDISK_MAX_BYTES=131072    # 256 sectors × 512 — must match RAMDISK_TOTAL_SECTORS in stage2.s

# KERNEL_TARGET is only ever used below to build *paths* (target/$KERNEL_TARGET/release/...).
# It intentionally has NO .json suffix, because that's what `cargo build --target`
# names the output directory as regardless of whether a bare name or a .json path
# was passed on the command line.
KERNEL_TARGET=x86_64-unknown-none

# BUG FIX: this must NOT be passed bare to `cargo build --target`. As of Rust 1.62,
# "x86_64-unknown-none" (no .json) is ALSO the name of an official rustc built-in
# Tier-2 target, and Cargo resolves a bare name against built-ins first. That
# built-in target silently shadows our own x86_64-unknown-none.json in this repo
# (different data-layout, different default code-model) even though the file is
# sitting right here — no error, no warning, just a different target getting built.
# Passing the actual .json path forces Cargo to load *our* spec, unambiguously.
KERNEL_TARGET_SPEC="$(pwd)/x86_64-unknown-none.json"

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
# Ramdisk contents: assembled BEFORE building the kernel
# =========================================================================
echo "==> Assembling userspace test_program (static ELF64, no libc)"
nasm -f elf64 userspace/test_program.s -o test_program.o
ld -e _start -Ttext=0x8000010000 -o test_program test_program.o
rm -f test_program.o

echo "==> Creating Ramdisk (rootfs.tar)"
rm -rf rootfs
mkdir -p rootfs/bin
echo "Hello from mitosOS in-memory filesystem!" > rootfs/test.txt
cp test_program rootfs/bin/test_program
tar -cf rootfs.tar -C rootfs bin/test_program test.txt
truncate -s "$RAMDISK_MAX_BYTES" rootfs.tar

echo "==> Building kernel ($KERNEL_TARGET, spec: $KERNEL_TARGET_SPEC)"
# -Z json-target-spec: as of a recent nightly Cargo change, loading a custom
# .json target spec (as opposed to a built-in target name) now requires this
# explicit opt-in, on top of -Z build-std (already set in .cargo/config.toml).
# Without it: "error: .json target specs require -Zjson-target-spec to be
# added to the cargo invocation".
cargo build --release -Z json-target-spec --target "$KERNEL_TARGET_SPEC"

llvm-objcopy -O binary target/x86_64-unknown-none/release/mitosos kernel.bin

KERNEL_ELF=$(find "target/$KERNEL_TARGET/release" -maxdepth 1 -type f -executable ! -name "*.d" | head -n1)
if [ -z "$KERNEL_ELF" ]; then
    echo "ERROR: couldn't find built kernel binary in target/$KERNEL_TARGET/release" >&2
    exit 1
fi

# --- FIX: Convert the compiled ELF into a flat binary using rust-objcopy ---
echo "==> Flattening kernel ELF into raw machine code binary"
rust-objcopy --strip-all -O binary "$KERNEL_ELF" kernel.bin

KERNEL_SIZE=$(stat -c%s kernel.bin 2>/dev/null || stat -f%z kernel.bin)
if [ "$KERNEL_SIZE" -gt "$KERNEL_MAX_BYTES" ]; then
    echo "ERROR: kernel.bin is $KERNEL_SIZE bytes, exceeds ${KERNEL_MAX_BYTES}-byte budget" >&2
    echo "       bump KERNEL_TOTAL_SECTORS in stage2.s if intentional" >&2
    exit 1
fi
truncate -s "$KERNEL_MAX_BYTES" kernel.bin

echo "==> Building disk image (stage1 + stage2 + kernel + ramdisk)"
rm -f disk.img
cat stage1.bin stage2.bin kernel.bin rootfs.tar > disk.img

# Sanity check: verify the MBR boot signature landed exactly where the BIOS
# expects it (offset 510-511 of the final disk image), so a bad build shows
# up here with a clear error instead of as a silent QEMU boot timeout later.
SIG=$(od -An -tx1 -j 510 -N 2 disk.img | tr -d ' \n')
if [ "$SIG" != "55aa" ]; then
    echo "ERROR: disk.img boot signature is 0x$SIG, expected 0x55aa at offset 510" >&2
    echo "       disk.img will not be recognized as bootable by BIOS." >&2
    exit 1
fi
echo "==> Verified: disk.img boot signature OK (0x55AA @ offset 510)"

DISK_SIZE=$(stat -c%s disk.img 2>/dev/null || stat -f%z disk.img)
echo "==> Done: disk.img ready ($DISK_SIZE bytes, $((DISK_SIZE / 512)) sectors)"
echo "    Test with: qemu-system-x86_64 -drive format=raw,file=disk.img"
