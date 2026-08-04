[bits 16]
[org 0x7c00]

STAGE2_LOAD_SEGMENT equ 0x0000
STAGE2_LOAD_OFFSET  equ 0x8000
STAGE2_SECTOR_COUNT equ 64      ; 32KB budget for stage2 -- build.sh must enforce this

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    sti

    mov [0x0500], dl        ; BIOS passes boot drive number in DL

    ; --- Boot checkpoint: stage1 is alive at 0x7c00 ---
    call print_a

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

    ; --- Boot checkpoint: stage2 read succeeded, about to jump to it ---
    call print_1

    jmp STAGE2_LOAD_SEGMENT:STAGE2_LOAD_OFFSET

disk_error:
    ; Mirror the failure to serial too -- the INT 0x10 print below is
    ; invisible whenever QEMU runs with -display none (i.e. every CI
    ; run), so without this a disk-read failure here looks identical
    ; to a silent triple fault in the captured serial log.
    call print_e
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

; --- Minimal serial checkpoint helpers (COM1, port 0x3f8) ---
; QEMU's chardev-backed 16550 model transmits whatever hits the THR
; immediately -- it doesn't emulate baud/line-control timing -- so
; these work even this early, before any UART setup exists. Each one
; preserves every register it touches, so it's safe to call from
; anywhere without disturbing DL (boot drive number), SI (DAP
; pointer), CX, etc.
print_a:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, 'A'
    out dx, al
    pop dx
    pop ax
    ret

print_1:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, '1'
    out dx, al
    pop dx
    pop ax
    ret

print_e:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, 'e'
    out dx, al
    pop dx
    pop ax
    ret

err_msg: db "Disk read failed", 0

align 4
dap:
    db 0x10                    ; packet size
    db 0                       ; reserved
    dw STAGE2_SECTOR_COUNT     ; sectors to read
    dw STAGE2_LOAD_OFFSET      ; dest offset
    dw STAGE2_LOAD_SEGMENT     ; dest segment
    dq 1                       ; start LBA (sector 1 -- right after boot sector)

times 510-($-$$) db 0
dw 0xaa55
