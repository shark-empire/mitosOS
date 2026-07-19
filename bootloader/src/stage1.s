 [bits 16]
[org 0x7c00]

STAGE2_LOAD_SEGMENT equ 0x0000
STAGE2_LOAD_OFFSET  equ 0x8000
STAGE2_SECTOR_COUNT equ 64      ; 32KB budget for stage2 — build.sh must enforce this

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    sti

    mov [0x0500], dl        ; BIOS passes boot drive number in DL

    ; Check BIOS supports INT 13h extensions (LBA reads)
    mov ah, 0x41
    mov bx, 0x55aa
    int 0x13
    jc disk_error

    ; Load stage2 via extended (LBA) disk read
    mov si, dap
    mov ah, 0x42
    mov dl, [0x0500]
    int 0x13
    jc disk_error

    jmp STAGE2_LOAD_SEGMENT:STAGE2_LOAD_OFFSET

disk_error:
    mov si, err_msg
.print:
    lodsb
    or al, al
    jz .halt
    mov ah, 0x0e
    int 0x10
    jmp .print
.halt:
    cli
    hlt
    jmp $


err_msg: db "Disk read failed", 0

align 4
dap:
    db 0x10                    ; packet size
    db 0                       ; reserved
    dw STAGE2_SECTOR_COUNT     ; sectors to read
    dw STAGE2_LOAD_OFFSET      ; dest offset
    dw STAGE2_LOAD_SEGMENT     ; dest segment
    dq 1                       ; start LBA (sector 1 — right after boot sector)

times 510-($-$$) db 0
dw 0xaa55
