[bits 64]

section .text.boot
global _start
extern kmain
extern __bss_start
extern __bss_end

_start:
    ; 1. Safe 64-bit Stack Setup
    ; Use LEA with RIP-relative addressing to avoid 32-bit absolute truncation
    lea rsp, [rel stack_top]

    ; 2. Preserve Bootloader Arguments!
    ; Bootloaders (Multiboot2, Limine, UEFI) pass boot info in registers (RDI, RSI, RAX, RBX, etc.).
    ; We MUST save them before our BSS loop and serial code clobbers them.
    ; Pushing 8 registers (64 bytes) also perfectly maintains 16-byte stack alignment.
    push rdi
    push rsi
    push rdx
    push rcx
    push r8
    push r9
    push rax
    push rbx

    ; --- Boot checkpoint 'Z' ---
    mov dx, 0x3f8
    mov al, 'Z'
    out dx, al

    ; 3. Zero the BSS (Position-Independent)
    lea rdi, [rel __bss_start]   ; Use RIP-relative addressing for externs
    lea rcx, [rel __bss_end]
    sub rcx, rdi                 ; RCX = size of BSS in bytes
    xor eax, eax                 ; Clear EAX (AL = 0 for stosb)
    cld                          ; Ensure forward string operations
    rep stosb                    ; Zero the BSS byte-by-byte

    ; --- Boot checkpoint 'Y' ---
    mov dx, 0x3f8
    mov al, 'Y'
    out dx, al

    ; 4. Restore Bootloader Arguments for Rust's kmain
    pop rbx
    pop rax
    pop r9
    pop r8
    pop rcx
    pop rdx
    pop rsi
    pop rdi

    ; 5. Call Kernel
    ; The System V AMD64 ABI requires RSP to be 16-byte aligned BEFORE the 'call' instruction.
    ; Because we pushed/popped symmetrically from an aligned 'stack_top', we are perfectly aligned.
    call kmain

.hang:
    cli
    hlt
    jmp .hang

section .bss.stack
align 16
    resb 16384
stack_top:
