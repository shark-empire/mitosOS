[bits 64]

section .text.boot
global _start
extern kmain
extern __bss_start
extern __bss_end

_start:
    mov rsp, stack_top

    mov rdi, __bss_start
    mov rcx, __bss_end
    sub rcx, rdi
    xor eax, eax
    cld
    rep stosb

    call kmain

.hang:
    cli
    hlt
    jmp .hang

section .bss.stack
align 16
    resb 16384
stack_top:
