[bits 16]         ; Start in 16-bit Real Mode
[org 0x7c00]      ; BIOS loads bootloader here

_start:
    ; 1. Setup Segment Registers
    xor ax, ax
    mov ds, ax
    mov es, ax
    
    ; 2. (You will eventually need code here to enable A20 line, 
    ;     setup GDT, and switch to 32-bit Protected Mode)
    
    ; Placeholder: Just call a routine or hang
    jmp $

times 510-($-$$) db 0
dw 0xaa55         ; Magic boot number
