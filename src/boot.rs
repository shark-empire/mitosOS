//! The very first code that runs, before any Rust runtime exists.
//!
//! Embedded via `global_asm!` instead of a separate assembled `.s` file —
//! one less moving part in the build, and it's stable Rust either way.

use core::arch::global_asm;

global_asm!(
    r#"
.section ".text._start"

.global _start

_start:
    // The Pi's firmware boots all 4 cores into this same image at once.
    // Only core 0 continues; cores 1-3 park themselves forever until a
    // future SMP phase wakes them deliberately.
    mrs x0, mpidr_el1
    and x0, x0, #0xff
    cbnz x0, park

    // Stack grows down from our own load address (0x80000); everything
    // below that is unused low RAM, which is plenty for now.
    ldr x1, =_start
    mov sp, x1

    // Zero .bss before any Rust code runs — Rust assumes zero-initialised
    // statics are actually zero.
    ldr x1, =__bss_start
    ldr x2, =__bss_end
zero_bss:
    cmp x1, x2
    b.eq call_kernel
    str xzr, [x1], #8
    b zero_bss

call_kernel:
    bl kernel_main
    // kernel_main never returns, but park here defensively just in case.

park:
    wfe
    b park
"#
);
