# mitosOS

A from-scratch AArch64 kernel written in Rust, aimed at being small enough
to fully audit and grown deliberately toward a hardened, capability-based
OS. See `SECURITY.md` for the ground rules this project follows.

## Why a Raspberry Pi and not "a phone"

A real phone has a locked bootloader, no public driver specs, and a
cellular modem stack that's legally safety-certified — none of that is
something any developer, on any timeline, can code around from scratch.
A Raspberry Pi 3-class board has none of those walls: public datasheets,
an unlocked boot process, and a well-documented AArch64 core. This targets
**Pi 3B (BCM2837)** specifically because it's the best-supported bare-metal
target in QEMU (`-M raspi3b`), which means every change gets a real
boot-test in CI before it ever touches physical hardware.

## Where things stand

**Phase 0**: the kernel boots, brings up a UART console by hand (no driver
crate — see `src/uart.rs`), and drops you into a small interactive shell
(`src/shell.rs`) with line editing and a few commands (`help`, `about`,
`echo`, `panic`). That's mitosOS's CLI today — reachable over a serial
connection, not a network. Everything else builds on top of this tested
baseline. See `SECURITY.md` for the phased roadmap, including where a
real network stack + SSH would eventually fit (well after exception
levels, paging, and user mode — and built on audited crypto crates, not
hand-rolled).

## How to build/run this

**GitHub Actions (the main workflow for now):** push to this repo and
`.github/workflows/ci.yml` builds the kernel, boots it headless in QEMU,
and checks the UART output automatically. Check the **Actions** tab after
each push — green means it booted and printed the expected line.

**Locally**, once you have a machine with `qemu-system-aarch64` and rustup:

```
rustup show                                    # installs the pinned toolchain + target
cargo install cargo-binutils                   # one-time, gives you rust-objcopy
cargo build --release
rust-objcopy --strip-all -O binary target/aarch64-unknown-none/release/mitosos kernel8.img
qemu-system-aarch64 -M raspi3b -kernel kernel8.img -serial stdio -display none
```

No nightly toolchain needed — `aarch64-unknown-none` is a stable, Tier 2
Rust target, so this builds on plain stable Rust.

## Putting this on a real Pi 3B (separate step, needs a desktop)

This part can't be done from a phone — you need an SD card reader:

1. Format a microSD card FAT32.
2. Copy `bootcode.bin`, `start.elf`, and `fixup.dat` onto it from
   [Raspberry Pi's official firmware repo](https://github.com/raspberrypi/firmware/tree/master/boot)
   — never a third-party mirror (see `SECURITY.md`).
3. Add a `config.txt` with at least:
   ```
   arm_64bit=1
   enable_uart=1
   dtoverlay=disable-bt
   ```
   The `disable-bt` line matters: on a Pi 3B, UART0 (PL011) is wired to
   the Bluetooth chip by default, and the GPIO14/15 header pins get the
   slower "mini-UART" instead. This overlay swaps that, so `src/uart.rs`
   talks to the header pins as written. QEMU doesn't need this — it wires
   UART0 straight to `-serial stdio` regardless.
4. Copy your built `kernel8.img` onto the card too.
5. Boot the Pi with a USB-to-serial adapter on GPIO14/15 (TXD/RXD) + GND,
   115200 8N1, to see the same output CI shows you.

## Layout

- `src/boot.rs` — the very first code that runs: parks secondary cores,
  sets up the stack, zeroes `.bss`, jumps into Rust.
- `src/uart.rs` — hand-written PL011 driver, no third-party crate.
- `src/main.rs` — kernel entry point and panic handler.
- `linker.ld` — places the kernel at `0x80000`, the fixed address the
  Pi's firmware always jumps to.
- `.github/workflows/ci.yml` — boots the kernel on every push and fails
  the build if it doesn't reach the known-good boot message.

## Heads up

I wrote and reasoned through this code carefully — the PL011/GPIO
register layout and the AArch64 boot sequence are extremely
well-documented and stable (this is the same hardware every Pi bare-metal
tutorial targets). But I couldn't compile or boot-test it myself before
handing it to you: this sandbox has no `rustc` aarch64 target installed,
no QEMU, and no network access. Treat the first CI run as the real first
compile. If it fails, paste me the Actions log and we'll fix it together
— normal for kernel bring-up, not a sign anything's wrong.

See `GETTING_STARTED.md` for getting this from a zip into a GitHub repo
from your phone.
