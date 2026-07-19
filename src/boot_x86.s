[bits 64]

section .text.boot
global _start
extern kernel_main
extern __bss_start
extern __bss_end

_start:
    ; Own dedicated kernel stack, rather than relying on the
    ; bootloader's temporary one long-term
    mov rsp, stack_top

    ; Zero .bss ourselves — the linker never emits bytes for a NOLOAD
    ; section, and stage2.s only copies the file's raw bytes, so this
    ; is the kernel's own responsibility now that Limine isn't doing it
    mov rdi, __bss_start
    mov rcx, __bss_end
    sub rcx, rdi
    xor eax, eax
    cld
    rep stosb

    call kernel_main

.hang:
    cli
    hlt
    jmp .hang

section .bss.stack
align 16
    resb 16384          ; 16KB kernel stack
stack_top:
