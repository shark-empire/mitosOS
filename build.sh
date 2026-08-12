#!/usr/bin/env bash
# build.sh -- builds mitosOS and packages it as a Limine-bootable ISO
# (mitosos.iso), bootable both as a BIOS CD/USB image and as a UEFI
# one. limine.conf offers the same kernel two ways -- natively via
# Limine, and via Multiboot2 (GRUB, QEMU's `-kernel`, ...) -- see
# src/limine.rs and src/boot_multiboot2.s.
#
# One-time setup this depends on and does NOT do itself: run
# ./fetch_limine.sh first (needs network; see that file for why this
# is a separate step).
set -euo pipefail
cd "$(dirname "$0")"

if [ ! -x limine/limine ]; then
    echo "ERROR: ./limine/limine (the Limine host tool) not found." >&2
    echo "       Run ./fetch_limine.sh first -- it needs network access this" >&2
    echo "       build script deliberately doesn't assume you have." >&2
    exit 1
fi

if ! command -v xorriso >/dev/null 2>&1; then
    echo "ERROR: xorriso not found (needed to build the ISO)." >&2
    echo "       Debian/Ubuntu: sudo apt install xorriso" >&2
    echo "       Fedora:        sudo dnf install xorriso" >&2
    echo "       macOS:         brew install xorriso" >&2
    exit 1
fi

# KERNEL_TARGET is only ever used below to build *paths*
# (target/$KERNEL_TARGET/release/...). It intentionally has NO .json
# suffix, because that's what `cargo build --target` names the output
# directory as regardless of whether a bare name or a .json path was
# passed on the command line.
KERNEL_TARGET=x86_64-unknown-none

# BUG FIX: this must NOT be passed bare to `cargo build --target`. As of Rust
# 1.62, "x86_64-unknown-none" (no .json) is ALSO the name of an official
# rustc built-in Tier-2 target, and Cargo resolves a bare name against
# built-ins first. That built-in target silently shadows our own
# x86_64-unknown-none.json in this repo (different data-layout, different
# default code-model) even though the file is sitting right here -- no
# error, no warning, just a different target getting built. Passing the
# actual .json path forces Cargo to load *our* spec, unambiguously.
KERNEL_TARGET_SPEC="$(pwd)/x86_64-unknown-none.json"

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

echo "==> Building kernel ($KERNEL_TARGET, spec: $KERNEL_TARGET_SPEC)"
# boot_x86.s and boot_multiboot2.s (NASM syntax) are assembled and
# linked in automatically here by build.rs -- see its file header --
# not by this script; nasm still needs to be installed and on PATH
# for that to succeed.
# -Z json-target-spec: as of a recent nightly Cargo change, loading a custom
# .json target spec (as opposed to a built-in target name) now requires this
# explicit opt-in, on top of -Z build-std (already set in .cargo/config.toml).
# Without it: "error: .json target specs require -Zjson-target-spec to be
# added to the cargo invocation".
cargo build --release -Z json-target-spec --target "$KERNEL_TARGET_SPEC"

KERNEL_ELF=$(find "target/$KERNEL_TARGET/release" -maxdepth 1 -type f -executable ! -name "*.d" | head -n1)
if [ -z "$KERNEL_ELF" ]; then
    echo "ERROR: couldn't find built kernel binary in target/$KERNEL_TARGET/release" >&2
    exit 1
fi

# The ELF is kept as-is -- no objcopy-to-flat-binary step. Limine loads
# ELF program headers directly; flattening would throw away the
# section/segment structure linker_x86.ld specifically arranges (see
# its file header comment).
echo "==> Staging ISO contents"
rm -rf iso_root
mkdir -p iso_root/boot/limine iso_root/EFI/BOOT

qemu-system-x86_64 -kernel "$KERNEL_ELF"


cp "$KERNEL_ELF" iso_root/boot/mitosos.elf
cp rootfs.tar iso_root/boot/rootfs.tar
cp limine.conf iso_root/boot/limine/limine.conf
cp limine/limine-bios.sys iso_root/boot/limine/
cp limine/limine-bios-cd.bin iso_root/boot/limine/
cp limine/limine-uefi-cd.bin iso_root/boot/limine/
cp limine/BOOTX64.EFI iso_root/EFI/BOOT/

echo "==> Building ISO (BIOS/UEFI hybrid)"
rm -f mitosos.iso
xorriso -as mkisofs -R -r -J -b boot/limine/limine-bios-cd.bin \
        -no-emul-boot -boot-load-size 4 -boot-info-table -hfsplus \
        -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin \
        -efi-boot-part --efi-boot-image --protective-msdos-label \
        iso_root -o mitosos.iso

echo "==> Installing Limine's BIOS stage 2 into the ISO"
./limine/limine bios-install mitosos.iso

ISO_SIZE=$(stat -c%s mitosos.iso 2>/dev/null || stat -f%z mitosos.iso)
echo "==> Done: mitosos.iso ready ($ISO_SIZE bytes)"
echo "    Test (BIOS):  qemu-system-x86_64 -cdrom mitosos.iso"
echo "    Test (UEFI):  qemu-system-x86_64 -cdrom mitosos.iso -bios /usr/share/ovmf/OVMF.fd"
