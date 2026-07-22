; Minimal freestanding ELF64 test program for mitosOS.
; Halts in a loop -- exists purely to exercise the VFS lookup -> ELF
; load path end-to-end. No libc, no dependencies.
BITS 64

GLOBAL _start

SECTION .text
_start:
.hang:
    hlt
    jmp .hang
