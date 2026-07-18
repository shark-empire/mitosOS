[bits 16]
[extern rust_main]

global _start
_start:
    ; 1. Setup segments and stack (real mode)
    cli
    mov ax, 0
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    ; 2. Enable A20 line — without this, memory above 1MB
    ;    wraps around and Rust code will read/write garbage
    in al, 0x92
    or al, 2
    out 0x92, al

    ; 3. Load the GDT (required before entering protected mode)
    lgdt [gdt_descriptor]

    ; 4. Set CR0.PE to enter protected mode
    mov eax, cr0
    or eax, 1
    mov cr0, eax

    ; 5. Far jump flushes the prefetch queue and loads CS
    ;    with the 32-bit code selector — this is what actually
    ;    switches the CPU into 32-bit instruction decoding
    jmp CODE_SEG:protected_mode_start

[bits 32]
protected_mode_start:
    ; 6. Reload remaining segment registers with the data selector
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    ; 7. Fresh 32-bit stack (0x90000 is free low memory, below 1MB)
    mov esp, 0x90000

    ; 8. Now safe to call into Rust
    call rust_main

    ; 9. Halt if it ever returns
    cli
    hlt
    jmp $

; --------------------------------------------------
; Global Descriptor Table — flat model, full 4GB
; code and data segments overlapping at base 0
; --------------------------------------------------
gdt_start:
gdt_null:
    dd 0x0
    dd 0x0

gdt_code:
    dw 0xffff       ; limit (low)
    dw 0x0          ; base (low)
    db 0x0          ; base (middle)
    db 10011010b    ; access: present, ring 0, code, executable, readable
    db 11001111b    ; flags: 4K granularity, 32-bit + limit (high)
    db 0x0          ; base (high)

gdt_data:
    dw 0xffff
    dw 0x0
    db 0x0
    db 10010010b    ; access: present, ring 0, data, writable
    db 11001111b
    db 0x0

gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1  ; size
    dd gdt_start                ; address

CODE_SEG equ gdt_code - gdt_start
DATA_SEG equ gdt_data - gdt_start

; --------------------------------------------------
; Padding to 512 bytes for BIOS
; --------------------------------------------------
times 510-($-$$) db 0
dw 0xaa55
