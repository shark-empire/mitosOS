// Minimal freestanding ELF64 test program for mitosOS (AArch64).
// AArch64 equivalent of test_program.s -- exists purely to exercise the
// VFS lookup -> ELF load -> EL0 spawn path end to end. No libc, no
// dependencies.
//
// x86_64's version uses `hlt`, a privileged instruction that faults
// unconditionally at ring 3. AArch64 has no single instruction that's
// "privileged" in quite that same sense (WFI/WFE's trap behavior is
// configurable via SCTLR_EL1), but any EL1-only system register is
// unconditionally inaccessible from EL0 in either direction -- so
// reading one is the cleanest equivalent: guaranteed to trap, no
// configuration-dependent behavior to worry about.
.section .text
.global _start
_start:
1:
    mrs x0, sctlr_el1   // EL1-only register -- always traps at EL0
    b   1b
