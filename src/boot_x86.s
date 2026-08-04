[bits 64]

section .text.boot
global _start
extern kmain
extern __bss_start
extern __bss_end

_start:
    mov rsp, stack_top

    ; --- Boot checkpoint: the Rust-linked kernel image's own _start
    ; is executing. If the bootloader trail ends at 'U' and this
    ; never prints, the fault is in the jump/handoff itself. ---
    push rax
    push rdx
    mov dx, 0x3f8
    mov al, 'Z'
    out dx, al
    pop rdx
    pop rax

    mov rdi, __bss_start
    mov rcx, __bss_end
    sub rcx, rdi
    xor eax, eax
    cld
    rep stosb

    ; --- Boot checkpoint: BSS zeroed, about to call into Rust (kmain).
    ; If 'Z' prints but this doesn't, the BSS-clear loop itself faulted
    ; -- worth comparing __bss_start/__bss_end against linker_x86.ld. ---
    push rax
    push rdx
    mov dx, 0x3f8
    mov al, 'Y'
    out dx, al
    pop rdx
    pop rax

    call kmain

.hang:
    cli
    hlt
    jmp .hang

section .bss.stack
align 16
    resb 16384
stack_top:
