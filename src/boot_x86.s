[bits 64]

section .text.boot
global _start
extern kmain
extern __bss_start
extern __bss_end

_start:
    ; 1. Preserve bootloader-provided arguments (Multiboot2/Limine/UEFI
    ; conventions pass boot info in RDI, RSI, RDX, RCX, R8, R9, RAX,
    ; RBX) in registers nothing below touches.
    ;
    ; This has to happen in registers, not on the stack: we haven't set
    ; up RSP yet, and `stack_top` (see section .bss.stack below) lives
    ; *inside* the BSS region the next step zeroes -- pushing to it now
    ; and popping after the zero loop would just mean the loop
    ; overwrites what we pushed, handing kmain zeroed garbage instead
    ; of its real arguments.
    ;
    ; Only 4 of the 8 need relaying: RDI/RCX/RAX are about to get
    ; overwritten by the `rep stosb` BSS-zero loop below, and RDX gets
    ; its low 16 bits clobbered by both checkpoint prints' `mov dx,
    ; 0x3f8`. RSI, R8, R9 and RBX are never touched below, so they
    ; still hold their original values by the time `call kmain` runs
    ; further down -- no relay needed for those four.
    mov r10, rdi
    mov r11, rcx
    mov r12, rax
    mov r13, rdx

    ; --- Boot checkpoint 'Z' ---
    mov dx, 0x3f8
    mov al, 'Z'
    out dx, al

    ; 2. Zero the BSS (Position-Independent)
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

    ; 3. NOW it's safe to move onto the kernel's own boot stack -- the
    ; BSS-zeroing above is done, so this no longer overlaps with it.
    ; RIP-relative so this is correct regardless of where the
    ; bootloader physically loaded us, as long as we're executing from
    ; our linked address (we are: stage2.s jumps to
    ; KERNEL_VIRT_LOAD_ADDR, matching linker_x86.ld exactly).
    lea rsp, [rel stack_top]

    ; 4. Restore bootloader arguments for Rust's kmain. kmain() doesn't
    ; currently take any parameters, so this is presently unobserved --
    ; kept correct anyway so a future boot-info parameter doesn't
    ; silently receive clobbered registers instead of real values.
    mov rdi, r10
    mov rcx, r11
    mov rax, r12
    mov rdx, r13

    ; 5. Call Kernel
    ; The System V AMD64 ABI requires RSP to be 16-byte aligned BEFORE
    ; the 'call' instruction. `stack_top` is 16-byte aligned (see
    ; `align 16` below) and nothing has touched RSP since, so we are.
    call kmain

.hang:
    cli
    hlt
    jmp .hang

section .bss.stack
align 16
    resb 16384
stack_top:
