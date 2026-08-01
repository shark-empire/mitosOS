// src/boot.s
.section .text.boot
.global _start

_start:
    // QEMU's raspi3b (mirroring real BCM2837 firmware) releases *all
    // four* cores at this exact same address when loaded via
    // `-kernel` -- there's no armstub8.s spin-table stub in front of
    // it to hold cores 1-3 back the way real GPU firmware normally
    // would. This kernel has no SMP support anywhere downstream (one
    // shared 64KB boot stack, a single-core scheduler in task.rs), so
    // without this check all 4 cores fall through to `bl kmain`
    // together and start stomping on each other's stack frames within
    // the first few calls. That's indistinguishable from a silent
    // hang -- corrupted return addresses send execution into
    // undefined memory before any core gets as far as the first UART
    // write, so kernel_main's "Boot OK" print (and even mmu.rs's own
    // earlier "MMU bring-up starting..." print) never happens.
    // Aff0 (mpidr_el1 bits 1:0) is the core number on this single-
    // cluster SoC -- 0-3, no Aff1+ decoding needed.
    mrs x0, mpidr_el1
    and x0, x0, #3
    cbz x0, primary_core

park_secondary:
    wfe
    b   park_secondary

primary_core:
    // Real Pi 3 firmware (and QEMU's raspi3b, which mirrors it) hands
    // a 64-bit kernel off at EL2, not EL1 -- occasionally EL3,
    // depending on the boot path. Everything downstream of this file
    // (VBAR_EL1 in interrupts.rs, SCTLR_EL1/TCR_EL1/TTBR0_EL1 in
    // mmu.rs, the EL1 physical timer and DAIF-based IRQ masking in
    // interrupts.rs/shell.rs) is EL1 state: writing it from a higher
    // EL "succeeds" with no error, but has zero effect on code that's
    // still actually executing up there, and any exception taken
    // before we truly reach EL1 uses that higher EL's own
    // (never-initialized) vector table instead of the one
    // interrupts.rs builds -- which looks exactly like "the shell
    // never receives typed input and no background task ever runs",
    // since the timer tick that's supposed to drive both never
    // actually reaches our handlers. See SECURITY.md's Phase 1
    // checklist -- this is the "EL2->EL1 drop" line item.
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #3
    b.eq el3_to_el2
    cmp x0, #2
    b.eq el2_to_el1
    b   el1_entry            // already at EL1 -- nothing to drop

el3_to_el2:
    // RW=1: EL2 (and everything below it) runs AArch64, not AArch32.
    // NS=1: land in Non-secure state -- the world QEMU/real firmware
    // leaves a normal kernel in.
    mov x0, #0x401            // SCR_EL3.{RW,NS}
    msr scr_el3, x0
    mov x0, #0x3c9             // target EL2h, DAIF all masked
    msr spsr_el3, x0
    adr x0, el2_to_el1
    msr elr_el3, x0
    eret

el2_to_el1:
    // Let EL1/EL0 read the physical counter and program the physical
    // timer (interrupts.rs's cntp_tval_el0/cntp_ctl_el0) directly --
    // both trap-to-EL2 bits default to 1 (trapping) on reset, and a
    // trap here has nowhere to go once EL2 is no longer running
    // anything.
    mrs x0, cnthctl_el2
    orr x0, x0, #3             // EL1PCTEN | EL1PCEN
    msr cnthctl_el2, x0
    msr cntvoff_el2, xzr

    // HCR_EL2.RW=1: EL1 runs AArch64. Everything else (IMO/FMO/AMO,
    // ...) stays 0, so IRQ/FIQ/SError taken at EL1 route to EL1's own
    // VBAR_EL1 like an ordinary non-virtualized kernel, not to EL2 --
    // exactly what interrupts.rs's vector table assumes.
    mov x0, #0x80000000
    msr hcr_el2, x0

    // EL1 needs its own stack live before it runs a single
    // instruction -- set SP_EL1 here, from EL2, rather than leaving
    // it to whatever EL1h resets to.
    ldr x0, =stack_top
    msr sp_el1, x0

    mov x0, #0x3c5              // target EL1h, DAIF all masked
    msr spsr_el2, x0
    adr x0, el1_entry
    msr elr_el2, x0
    eret

el1_entry:
    // Actually at EL1 now (or started here, if firmware already
    // handed off at EL1). DAIF stays masked until kmain's
    // interrupts::enable_cpu_interrupts() explicitly clears the I bit
    // once the vector table, heap, and MMU are all ready.
    ldr x0, =stack_top
    mov sp, x0

    // Zero .bss -- this stack, the heap allocator's internal state,
    // and the physical frame bitmap all live in here as
    // zero-initialized statics. QEMU happens to zero-fill guest RAM
    // by default, which is the only reason skipping this has ever
    // gotten away with working; real hardware and other loaders make
    // no such promise. __bss_start/__bss_end come from linker_rpi.ld
    // and are both 8-byte aligned (the section itself is ALIGN(16),
    // and the end is explicitly `. = ALIGN(8)`), so a plain
    // store-and-advance-by-8 loop covers the whole range exactly.
    ldr x0, =__bss_start
    ldr x1, =__bss_end
bss_zero_loop:
    cmp x0, x1
    b.ge bss_zero_done
    str xzr, [x0], #8
    b   bss_zero_loop
bss_zero_done:

    bl kmain

    // If it ever returns, loop forever
    b .

.section .bss
.align 16
stack_bottom:
    .skip 65536 // Allocate 64KB for the stack
stack_top:
