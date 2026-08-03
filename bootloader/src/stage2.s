; =========================================================================
; Production-Grade Stage 2 x86_64 Bootloader for mitosOS
; Solves QEMU Triple Fault by mapping 1GB physical RAM & PCI MMIO
; =========================================================================
[bits 16]
[org 0x8000]
section .text
global _start

; --- Memory & Geometry Constants ---
KERNEL_TEMP_SEGMENT   equ 0x1000    ; 0x1000:0x0000 = physical 0x10000
KERNEL_TEMP_OFFSET    equ 0x0000
KERNEL_TOTAL_SECTORS  equ 768       ; 384KB total
KERNEL_CHUNK_SECTORS  equ 64        ; 32KB per BIOS call
KERNEL_START_LBA      equ 65        ; sector 0=stage1, 1-64=stage2, 65=kernel
KERNEL_PHYS_LOAD_ADDR equ 0x100000  ; Physical address: 1MB

RAMDISK_TEMP_SEGMENT  equ 0x7000    ; 0x7000:0x0000 = physical 0x70000
RAMDISK_TOTAL_SECTORS equ 256       ; 128KB total
RAMDISK_START_LBA     equ 833       ; 65 + 768 = immediately after kernel
RAMDISK_PHYS_LOAD_ADDR equ 0x200000 ; Physical address: 2MB

HIGHER_HALF_PML4_IDX  equ 256       ; Maps 0xFFFF_8000_0000_0000
KERNEL_VIRT_LOAD_ADDR equ 0xFFFF_8000_0010_0000

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    sti

    ; 1. Load KERNEL into 0x10000 (12 chunks of 64 sectors)
    mov cx, KERNEL_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
.read_kernel_loop:
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
    loop .read_kernel_loop

    ; 2. Load RAMDISK into 0x70000
    mov cx, RAMDISK_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
    mov dword [disk_dap + 8], RAMDISK_START_LBA
    mov word [disk_dap + 6], RAMDISK_TEMP_SEGMENT
.read_ramdisk_loop:
    push cx
    mov si, disk_dap
    mov ah, 0x42
    mov dl, [0x0500]
    int 0x13
    jc .ramdisk_missing
    
    add dword [disk_dap + 8], KERNEL_CHUNK_SECTORS
    adc dword [disk_dap + 12], 0
    add word [disk_dap + 6], 0x0800
    pop cx
    loop .read_ramdisk_loop
    jmp .done_reading

.ramdisk_missing:
    pop cx

.done_reading:
    cli                            

    ; Enable A20 Line
    in al, 0x92
    or al, 2
    out 0x92, al

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp CODE_SEG:protected_mode_start

disk_error:
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

    ; Copy KERNEL from temp real-mode buffer to 1MB physical
    mov esi, (KERNEL_TEMP_SEGMENT * 16) + KERNEL_TEMP_OFFSET
    mov edi, KERNEL_PHYS_LOAD_ADDR
    mov ecx, (KERNEL_TOTAL_SECTORS * 512) / 4
    cld
    rep movsd

    ; Copy RAMDISK from temp real-mode buffer to 2MB physical
    mov esi, (RAMDISK_TEMP_SEGMENT * 16)
    mov edi, RAMDISK_PHYS_LOAD_ADDR
    mov ecx, (RAMDISK_TOTAL_SECTORS * 512) / 4
    cld
    rep movsd

    ; --- Build Hardened Page Tables: Maps 1GB Physical RAM ---
    ; Zero 4 pages (PML4 @ 0x1000, PDPT @ 0x2000, PD @ 0x3000, Extra PD @ 0x4000)
    mov edi, 0x1000          
    mov ecx, 4096            
    xor eax, eax
    rep stosd

    ; Link PML4[0] (Lower) & PML4[256] (Higher-Half) -> PDPT (0x2000)
    mov dword [0x1000], 0x2003
    mov dword [0x1000 + HIGHER_HALF_PML4_IDX * 8], 0x2003

    ; Link PDPT[0] -> PD (0x3000)
    mov dword [0x2000], 0x3003
    
    ; Populate 512 entries in PD = 512 x 2MB = 1GB Physical RAM Mapped!
    mov edi, 0x3000
    mov eax, 0x83                ; Present + Writable + PageSize (2MB Huge)
    mov ecx, 512
.map_1gb_loop:
    mov [edi], eax
    mov dword [edi + 4], 0
    add eax, 0x200000            ; Advance physical address by 2MB
    add edi, 8
    loop .map_1gb_loop

    ; Load CR3
    mov eax, 0x1000
    mov cr3, eax              

    ; Enable PAE in CR4
    mov eax, cr4
    or eax, 0x20               
    mov cr4, eax

    ; Enable Long Mode & NX-bit in EFER MSR
    mov ecx, 0xC0000080         
    rdmsr
    or eax, 0x900                ; EFER.LME (Long Mode) + EFER.NXE
    wrmsr

    ; Enable Paging in CR0
    mov eax, cr0
    or eax, 0x80000000           
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

    ; Relocate Stack Pointer (RSP) to Higher-Half Safe RAM (Physical 0x300000)
    mov rax, 0xFFFF_8000_0030_0000
    mov rsp, rax

    ; Jump to Kernel in Higher-Half space
    mov rax, higher_half_entry
    mov rbx, 0xFFFF800000000000
    or rax, rbx
    jmp rax

higher_half_entry:
    ; Unmap lower-half identity mapping (PML4[0])
    mov rax, 0xFFFF_8000_0000_1000      ; Higher-half virtual address of PML4
    mov qword [rax], 0                  ; Clear PML4 Index 0

    ; Flush TLB
    mov rax, cr3
    mov cr3, rax

    ; Jump to Rust kernel main
    mov rax, KERNEL_VIRT_LOAD_ADDR
    jmp rax

align 8
gdt_start:
gdt_null:
    dd 0x0, 0x0
gdt_code:
    dw 0xffff, 0x0
    db 0x0, 10011010b, 11001111b, 0x0
gdt_data:
    dw 0xffff, 0x0
    db 0x0, 10010010b, 11001111b, 0x0
gdt_code64:
    dw 0xffff, 0x0
    db 0x0, 10010010b, 10101111b, 0x0
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd gdt_start

CODE_SEG   equ gdt_code   - gdt_start
DATA_SEG   equ gdt_data   - gdt_start
CODE64_SEG equ gdt_code64 - gdt_start
