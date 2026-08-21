#!/usr/bin/env bash
# fetch_limine.sh -- one-time setup, run this yourself, not part of
# build.sh.
#
# Downloads the `limine` host tool (the program that writes Limine's
# BIOS stage 2 into a disk image/ISO) and the prebuilt BIOS/UEFI
# bootloader stage binaries it ships alongside, into ./limine/.
# build.sh looks for that directory and stops with a clear
# error, pointing back here, if it isn't there yet.
#
# This needs network access, which is why it's a separate script:
# the sandbox this repo's Limine integration was written in doesn't
# have any, so this step could only be written, not run or verified,
# there. Run it yourself once (or again later to update Limine).
set -euo pipefail
cd "$(dirname "$0")"

if [ -d limine ]; then
    echo "==> ./limine already exists -- remove it first if you want to re-fetch"
    exit 0
fi

echo "==> Cloning Limine (binary release branch: prebuilt stages + portable host-tool source)"
# v11.x, not v9.x: v9.x's own PROTOCOL.md caps out at base revision 3
# ("so far, 4 base revisions are specified: 0 through 3" -- same as
# v8.x). Base revision 4 -- the one that guarantees ACPI tables land
# in an HHDM-mapped memory map region, which src/hal/acpi.rs and
# src/limine.rs now require -- only exists in the newer protocol
# spec v10.x/v11.x implement. Confirmed empirically too: booting
# against v9.x-binary, Limine itself reports back "loaded base
# revision: 3" even though the kernel requests 4, and PROTOCOL.md
# requires bootloaders to report the *actual* base revision used
# regardless of what was requested -- so that's not a kernel-side
# bug, it's this binary genuinely not implementing revision 4.
git clone https://github.com/limine-bootloader/limine.git --branch=v11.x-binary --depth=1


echo "==> Building the limine host tool"
make -C limine

echo "==> Done. ./limine/limine (host tool) and the prebuilt boot stage files are ready."
echo "    Run ./build.sh next."
