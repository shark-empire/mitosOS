; test_program.s
BITS 64

GLOBAL _start

SECTION .text
_start:
    ; Assuming standard x86_64 calling convention: RAX = syscall number
    mov rax, 99     ; Syscall 99: QEMU Debug Exit (for CI)
    mov rdi, 0      ; Arg 1: Exit code 0 (Success)
    
    syscall         ; (Or 'int 0x80' depending on how you set up your IDT/MSRs)

.hang:
    pause           ; Fallback in case the syscall fails
    jmp .hang
