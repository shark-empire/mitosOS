[bits 16]
[org 0x8000]
section .text
global _start

; --- Kernel Memory Layout ---
KERNEL_TEMP_SEGMENT   equ 0x1000    ; 0x1000:0x0000 = physical 0x10000
KERNEL_TEMP_OFFSET    equ 0x0000
KERNEL_TOTAL_SECTORS  equ 256       ; 128KB total (Kernel Max Size)
KERNEL_CHUNK_SECTORS  equ 64        ; 32KB per BIOS call
KERNEL_START_LBA      equ 65        ; sector 0=stage1, 1-64=stage2, 65=kernel
KERNEL_LOAD_ADDR      equ 0x100000  ; final home: 1MB

; --- Ramdisk Memory Layout ---
RAMDISK_TEMP_SEGMENT  equ 0x3000    ; 0x3000:0x0000 = physical 0x30000 (Immediately after temp kernel)
RAMDISK_TOTAL_SECTORS equ 256       ; 128KB total (Ramdisk Max Size - bump this if your tar gets bigger)
RAMDISK_START_LBA     equ 321       ; 65 + 256 = immediately after the kernel on disk
RAMDISK_LOAD_ADDR     equ 0x200000  ; final home: 2MB (Immediately after the loaded kernel)

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    sti                            ; Enable interrupts for BIOS disk services

    ; 1. Load the KERNEL into 0x10000 (4 chunks of 64 sectors)
    mov cx, KERNEL_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
.read_kernel_loop:
    push cx
    mov si, disk_dap
    mov ah, 0x42
    mov dl, [0x0500]               ; boot drive, stashed by stage1
    int 0x13
    jc disk_error
    
    add dword [disk_dap + 8], KERNEL_CHUNK_SECTORS
    adc dword [disk_dap + 12], 0
    add word [disk_dap + 6], 0x0800 ; Advance segment by 32KB
    pop cx
    loop .read_kernel_loop

    ; 2. Load the RAMDISK into 0x30000 (4 chunks of 64 sectors)
    mov cx, RAMDISK_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
    mov dword [disk_dap + 8], RAMDISK_START_LBA
    mov word [disk_dap + 6], RAMDISK_TEMP_SEGMENT
.read_ramdisk_loop:
    push cx
    mov si, disk_dap
    mov ah, 0x42
    mov dl, [0x0500]
    int 0x13
    jc disk_error
    
    add dword [disk_dap + 8], KERNEL_CHUNK_SECTORS
    adc dword [disk_dap + 12], 0
    add word [disk_dap + 6], 0x0800
    pop cx
    loop .read_ramdisk_loop

    cli                            ; Disable interrupts again before protected mode

    ; Enable A20
    in al, 0x92
    or al, 2
    out 0x92, al

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp CODE_SEG:protected_mode_start

disk_error:
    ; Print 'E' to COM1 serial port for CI/CD debugging
    mov dx, 0x3f8
    mov al, 'E'
    out dx, al
    cli
    hlt
    jmp $

align 4
disk_dap:
    db 0x10
    db 0
    dw KERNEL_CHUNK_SECTORS
    dw KERNEL_TEMP_OFFSET
    dw KERNEL_TEMP_SEGMENT
    dq KERNEL_START_LBA

[bits 32]
protected_mode_start:
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov esp, 0x90000

    ; Move the KERNEL from temp real-mode buffer to 1MB
    mov esi, (KERNEL_TEMP_SEGMENT * 16) + KERNEL_TEMP_OFFSET
    mov edi, KERNEL_LOAD_ADDR
    mov ecx, (KERNEL_TOTAL_SECTORS * 512) / 4
    cld
    rep movsd

    ; Move the RAMDISK from temp real-mode buffer to 2MB
    mov esi, (RAMDISK_TEMP_SEGMENT * 16)
    mov edi, RAMDISK_LOAD_ADDR
    mov ecx, (RAMDISK_TOTAL_SECTORS * 512) / 4
    cld
    rep movsd

    ; --- Build minimal page tables: identity-map the first 4MB ---
    mov edi, 0x1000          ; PML4 table
    mov ecx, 3072            ; zero 3 pages (PML4+PDPT+PD) = 12KB = 3072 dwords
    xor eax, eax
    rep stosd

    mov dword [0x1000], 0x2003   ; PML4[0] -> PDPT at 0x2000, present+writable
    mov dword [0x2000], 0x3003   ; PDPT[0] -> PD at 0x3000, present+writable
    
    ; Map first 2MB (0x0 to 0x1FFFFF) - Covers BIOS, Stage 1/2, and Kernel
    mov dword [0x3000], 0x83     
    
    ; Map second 2MB (0x200000 to 0x3FFFFF) - Covers Ramdisk
    mov dword [0x3008], 0x200083 

    mov eax, 0x1000
    mov cr3, eax              ; CR3 = PML4 physical address

    mov eax, cr4
    or eax, 0x20               ; CR4.PAE
    mov cr4, eax

    mov ecx, 0xC0000080         ; EFER MSR
    rdmsr
    or eax, 0x100                ; EFER.LME
    wrmsr

    mov eax, cr0
    or eax, 0x80000000           ; CR0.PG — activates long mode
    mov cr0, eax

    jmp CODE64_SEG:long_mode_start

[bits 64]
long_mode_start:
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov rsp, 0x90000

    jmp KERNEL_LOAD_ADDR

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
gdt_code64:
    dw 0xffff
    dw 0x0
    db 0x0
    db 10011010b     
    db 10101111b     
    db 0x0
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd gdt_start

CODE_SEG   equ gdt_code   - gdt_start
DATA_SEG   equ gdt_data   - gdt_start
CODE64_SEG equ gdt_code64 - gdt_start
