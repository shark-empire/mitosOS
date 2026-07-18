[bits 16]
[extern rust_main]
[extern _bss_start]
[extern _bss_end]

global _start
_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    ; Enable A20 — without this, memory above 1MB wraps and
    ; Rust code silently reads/writes the wrong address
    in al, 0x92
    or al, 2
    out 0x92, al

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp CODE_SEG:protected_mode_start

[bits 32]
protected_mode_start:
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov esp, 0x90000

    ; Zero .bss ourselves — raw binary output has no ELF loader
    ; to do this, so Rust statics would start as garbage otherwise
    cld
    mov edi, _bss_start
    mov ecx, _bss_end
    sub ecx, edi
    xor eax, eax
    rep stosb

    call rust_main

    cli
    hlt
    jmp $

gdt_start:
gdt_null:
    dd 0x0
    dd 0x0
gdt_code:
    dw 0xffff
    dw 0x0
    db 0x0
    db 10011010b
    db 11001111b
    db 0x0
gdt_data:
    dw 0xffff
    dw 0x0
    db 0x0
    db 10010010b
    db 11001111b
    db 0x0
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd gdt_start

CODE_SEG equ gdt_code - gdt_start
DATA_SEG equ gdt_data - gdt_start
