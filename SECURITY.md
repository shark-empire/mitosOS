# Security principles for mitosOS

This is a long-term hardened OS project, not a toy. These are the ground
rules for every change, starting from commit one.

## Non-negotiables

- **No `unsafe` you can't explain.** Every `unsafe` block gets a `# Safety`
  or `// Safety:` comment saying exactly why it's sound. If you can't write
  that comment, the code isn't ready to merge.
- **Minimal dependencies.** Every crate we pull in is attack surface and a
  supply-chain risk. This kernel has *zero* third-party crate dependencies
  — the UART driver in `src/uart.rs` is written by hand on purpose.
- **No feature we don't need yet.** Networking, storage, and multi-user
  support aren't "off by default" — they don't exist in the code at all
  until we deliberately design and add them. A code path that hasn't been
  written can't have a backdoor in it.
- **Every boot is tested.** `.github/workflows/ci.yml` boots the kernel in
  QEMU on every push and fails the build if it doesn't reach a known-good
  state. A kernel that doesn't boot never merges.
- **Memory safety by construction.** The kernel is Rust, `#![no_std]`, with
  panic=abort. The compiler rejects use-after-free, double-free, and most
  buffer overflows before the code ever runs.

## What "no backdoor" actually means here

No one can prove a nontrivial codebase has zero vulnerabilities, and
nobody should claim otherwise. What we can do, and will keep doing as
this grows:
- Keep the codebase small enough that one person can read all of it.
- Review every `unsafe` block as a security-critical diff, not a routine one.
- Keep a boot-time smoke test in CI so regressions get caught immediately.
- Add a threat-model note here before adding any new subsystem (paging,
  syscalls, drivers, storage, etc.) describing what it exposes and to whom.

## Trust boundary note specific to real Pi hardware

`bootcode.bin` / `start.elf` / the GPU-side firmware that loads
`kernel8.img` are closed-source binary blobs from the Raspberry Pi
Foundation, not something this project can audit or replace — that's the
one piece of the boot chain we don't control, same as every other Pi OS
(including Raspberry Pi OS itself). Only ever source those files from
Raspberry Pi's official firmware repository, never a third-party mirror.
Everything from `kernel8.img` onward is ours to audit.

## Roadmap

- [x] Phase 0 — boots to a UART "hello", CI boot-test in QEMU
- [ ] Phase 1 — exception levels (EL2->EL1 drop), exception vector table,
      physical frame allocator
- [ ] Phase 2 — MMU/paging hardening (W^X pages, guard pages)
- [ ] Phase 3 — user mode (EL0), syscall interface, capability-based
      access control (processes get explicit, minimal capabilities — not
      ambient root-style authority)
- [ ] Phase 4 — minimal userland
- [ ] Phase 5 — real-hardware bring-up on an actual Pi 3B (see README)
