[bits 16]
[extern rust_main]

global _start
_start:
    ; 1. Setup Stack
    cli
    mov ax, 0
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    ; 2. Call Rust
    call rust_main

    ; 3. Halt if it returns
    hlt
    jmp $

; 4. Padding to 512 bytes for BIOS
times 510-($-$$) db 0
dw 0xaa55
