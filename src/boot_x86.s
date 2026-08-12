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

    ; --- TEMP DIAGNOSTIC: dump the three computed addresses this code
    ; depends on, before the BSS loop (which depends on the first two)
    ; or the stack switch (which depends on the third) run. Expect
    ; sFFFF8000001XXXXX:eFFFF8000001XXXXX:tFFFF8000001XXXXX: with the
    ; e value >= the s value. Remove once the hang is found. ---
    mov dx, 0x3f8
    mov al, 's'
    out dx, al
    lea rax, [rel __bss_start]
    call print_hex64
    mov dx, 0x3f8
    mov al, ':'
    out dx, al
    mov al, 'e'
    out dx, al
    lea rax, [rel __bss_end]
    call print_hex64
    mov dx, 0x3f8
    mov al, ':'
    out dx, al
    mov al, 't'
    out dx, al
    lea rax, [rel stack_top]
    call print_hex64
    mov dx, 0x3f8
    mov al, ':'
    out dx, al
    ; --- end diagnostic ---

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
    ; our linked address (we are: Limine jumps straight here per
    ; ENTRY(_start), and boot_multiboot2.s's trampoline jumps here
    ; too, only after reaching that same linked address itself --
    ; matching linker_x86.ld exactly either way).
    lea rsp, [rel stack_top]

    ; 4. Restore bootloader arguments for Rust's kmain. On x86_64,
    ; kmain(arg0: u64, arg1: u64) -- see main.rs -- actually reads
    ; these now: a Multiboot2 boot relays its info pointer and magic
    ; through here (boot_multiboot2.s sets rdi/rsi right before
    ; jumping to this file), which boot_info::init uses to tell
    ; Multiboot2 apart from Limine (which guarantees every register
    ; zero at entry, so both arrive as 0 on that path).
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

; --- TEMP DIAGNOSTIC helper: prints RAX as 16 hex chars over COM1.
; Preserves every register it touches. Remove once the hang is found.
print_hex64:
    push rax
    push rbx
    push rcx
    push rdx
    mov rbx, rax
    mov cl, 60
.print_hex64_loop:
    mov rax, rbx
    shr rax, cl
    and al, 0x0F
    cmp al, 10
    jb .print_hex64_digit
    add al, 'A' - 10
    jmp .print_hex64_out
.print_hex64_digit:
    add al, '0'
.print_hex64_out:
    mov dx, 0x3f8
    out dx, al
    sub cl, 4
    jnc .print_hex64_loop
    pop rdx
    pop rcx
    pop rbx
    pop rax
    ret

section .bss.stack
align 16
    resb 16384
stack_top:
