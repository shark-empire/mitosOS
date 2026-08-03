[bits 16]
[org 0x8000]
section .text
global _start

; --- Kernel Memory Layout ---
KERNEL_TEMP_SEGMENT   equ 0x1000    ; 0x1000:0x0000 = physical 0x10000
KERNEL_TEMP_OFFSET    equ 0x0000
KERNEL_TOTAL_SECTORS  equ 768       ; 384KB total
KERNEL_CHUNK_SECTORS  equ 64        ; 32KB per BIOS call
KERNEL_START_LBA      equ 65        ; sector 0=stage1, 1-64=stage2, 65=kernel
KERNEL_PHYS_LOAD_ADDR equ 0x100000  ; Physical address: 1MB

; --- Ramdisk Memory Layout ---
RAMDISK_TEMP_SEGMENT  equ 0x7000    ; 0x7000:0x0000 = physical 0x70000
RAMDISK_TOTAL_SECTORS equ 256       ; 128KB total
RAMDISK_START_LBA     equ 833       ; 65 + 768 = immediately after kernel
RAMDISK_PHYS_LOAD_ADDR equ 0x200000 ; Physical address: 2MB

; --- Higher-Half Mapping Constants ---
HIGHER_HALF_PML4_IDX  equ 256       ; Maps 0xFFFF_8000_0000_0000
KERNEL_VIRT_LOAD_ADDR equ 0xFFFF_8000_0010_0000

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    sti                            ; Enable interrupts for BIOS disk services

    ; 1. Load the KERNEL into 0x10000 (12 chunks of 64 sectors)
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

    ; 2. Load the RAMDISK into 0x70000
    mov cx, RAMDISK_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
    mov dword [disk_dap + 8], RAMDISK_START_LBA
    mov word [disk_dap + 6], RAMDISK_TEMP_SEGMENT
.read_ramdisk_loop:
    push cx
    mov si, disk_dap
    mov ah, 0x42
    mov dl, [0x0500]
    int 0x13
    jc .ramdisk_missing            ; Graceful fallback
    
    add dword [disk_dap + 8], KERNEL_CHUNK_SECTORS
    adc dword [disk_dap + 12], 0
    add word [disk_dap + 6], 0x0800
    pop cx
    loop .read_ramdisk_loop
    jmp .done_reading

.ramdisk_missing:
    pop cx                         ; Clean the stack

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

    ; --- Build Page Tables: Higher-Half & Identity Mappings ---
    ; Zero 3 pages (PML4 @ 0x1000, PDPT @ 0x2000, PD @ 0x3000) = 12KB
    mov edi, 0x1000          
    mov ecx, 3072            
    xor eax, eax
    rep stosd

    ; Link PML4[0] (Temporary lower-half identity) -> PDPT (0x2000)
    mov dword [0x1000], 0x2003
    ; Link PML4[256] (Higher-half 0xFFFF_8000_0000_0000) -> PDPT (0x2000)
    mov dword [0x1000 + HIGHER_HALF_PML4_IDX * 8], 0x2003

    ; Link PDPT[0] -> PD (0x3000)
    mov dword [0x2000], 0x3003
    
    ; Map first 2MB (0x0 to 0x1FFFFF) - Huge Page (0x83 = Present + Writable + PS)
    mov dword [0x3000], 0x83     
    ; Map second 2MB (0x200000 to 0x3FFFFF) - Ramdisk
    mov dword [0x3008], 0x200083 

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
    or eax, 0x100               ; EFER.LME (Long Mode Enable)
    or eax, 0x800               ; EFER.NXE (No-Execute Enable)
    wrmsr

    ; Enable Paging in CR0 (Activates Long Mode)
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

    ; Relocate Stack Pointer (RSP) to Higher-Half
    mov rax, 0xFFFF_8000_000A_0000
    mov rsp, rax

    ; Jump to Kernel in Higher-Half space
    mov rax, higher_half_entry
    mov rbx, 0xFFFF800000000000         ; Apply the higher-half offset
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
