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

    ; --- Boot checkpoint: stage2 is alive at 0x8000 ---
    call print_2

    ; 1. Load the KERNEL into 0x10000 (12 chunks of 64 sectors)
    mov cx, KERNEL_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
.read_kernel_loop:
    push cx
    mov word [disk_dap + 2], KERNEL_CHUNK_SECTORS ; Re-init count (BIOS can overwrite)
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

    ; --- Boot checkpoint: kernel image fully read from disk ---
    call print_k

    ; 2. Load the RAMDISK into 0x70000
    mov cx, RAMDISK_TOTAL_SECTORS / KERNEL_CHUNK_SECTORS
    mov dword [disk_dap + 8], RAMDISK_START_LBA
    mov dword [disk_dap + 12], 0
    mov word [disk_dap + 6], RAMDISK_TEMP_SEGMENT
.read_ramdisk_loop:
    push cx
    mov word [disk_dap + 2], KERNEL_CHUNK_SECTORS
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
    pop cx                         ; Clean stack frame for failed loop iteration

.done_reading:
    cli                            

    ; --- Boot checkpoint: ramdisk phase done ---
    call print_r

    ; Enable A20 Line
    in al, 0x92
    or al, 2
    out 0x92, al

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax

    ; --- Boot checkpoint: A20 + GDT done ---
    call print_d

    jmp CODE_SEG:protected_mode_start

disk_error:
    mov dx, 0x3f8
    mov al, 'E'
    out dx, al
    cli
    hlt
    jmp $

print_2:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, '2'
    out dx, al
    pop dx
    pop ax
    ret

print_k:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, 'K'
    out dx, al
    pop dx
    pop ax
    ret

print_r:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, 'R'
    out dx, al
    pop dx
    pop ax
    ret

print_d:
    push ax
    push dx
    mov dx, 0x3f8
    mov al, 'D'
    out dx, al
    pop dx
    pop ax
    ret

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
    
    ; FIX: Move stack to 0x9F000 instead of 0x90000.
    ; Ramdisk buffer spans 0x70000-0x8FFFF. Pushing onto stack at 0x90000 
    ; grows downwards into 0x8FFFF and corrupts the ramdisk data!
    mov esp, 0x9F000

    ; --- Boot checkpoint: protected mode entered ---
    push eax
    push edx
    mov dx, 0x3f8
    mov al, 'P'
    out dx, al
    pop edx
    pop eax

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

    ; --- Boot checkpoint: kernel + ramdisk copied ---
    push eax
    push edx
    mov dx, 0x3f8
    mov al, 'M'
    out dx, al
    pop edx
    pop eax

    ; --- Build Page Tables ---
    mov edi, 0x1000          
    mov ecx, 4096            
    xor eax, eax
    rep stosd

    ; Link PML4[0] (Temporary lower-half identity) -> PDPT (0x2000)
    mov dword [0x1000], 0x2003
    ; Link PML4[256] (Higher-half) -> PDPT (0x2000)
    mov dword [0x1000 + HIGHER_HALF_PML4_IDX * 8], 0x2003

    ; Link PDPT[0] -> PD (0x3000)
    mov dword [0x2000], 0x3003
    
    ; Populate 512 entries in PD = 1GB Physical RAM (2MB Huge Pages)
    mov edi, 0x3000
    mov eax, 0x83                ; Present + Writable + PageSize (2MB Huge)
    mov ecx, 512
.map_1gb_loop:
    mov [edi], eax
    mov dword [edi + 4], 0
    add eax, 0x200000            ; Advance physical address by 2MB
    add edi, 8
    loop .map_1gb_loop

    ; --- Boot checkpoint: page tables built ---
    push eax
    push edx
    mov dx, 0x3f8
    mov al, 'G'
    out dx, al
    pop edx
    pop eax

    ; Load CR3
    mov eax, 0x1000
    mov cr3, eax              

    ; Enable PAE in CR4
    mov eax, cr4
    or eax, 0x620               
    mov cr4, eax

    ; Enable Long Mode & NX-bit in EFER MSR
    mov ecx, 0xC0000080         
    rdmsr
    or eax, 0x100               ; EFER.LME (Long Mode Enable)
    or eax, 0x800               ; EFER.NXE (No-Execute Enable)
    wrmsr

    ; --- Boot checkpoint: paging enabled, about to enter long mode ---
    push eax
    push edx
    mov dx, 0x3f8
    mov al, 'X'
    out dx, al
    pop edx
    pop eax

    ; FIX: Set CR0.PG (Bit 31) to activate Paging & Long Mode!
    ; Previously, bit 31 was omitted, causing execution to remain in 32-bit mode
    ; and faulting on the far jump to 64-bit code space.
    mov eax, cr0
    and eax, ~(1 << 2)          ; Clear EM
    or eax, (1 << 1) | (1 << 31) ; Set MP and PG (Paging Enable)
    mov cr0, eax

    jmp CODE64_SEG:long_mode_start

[bits 64]
long_mode_start:
    ; --- Boot checkpoint: long mode entered ---
    push rax
    push rdx
    mov dx, 0x3f8
    mov al, 'L'
    out dx, al
    pop rdx
    pop rax

    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    ; Relocate Stack Pointer (RSP) to Higher-Half Safe RAM
    mov rax, 0xFFFF_8000_0030_0000
    mov rsp, rax

    ; --- Boot checkpoint: jumping to higher-half alias ---
    push rax
    push rdx
    mov dx, 0x3f8
    mov al, 'H'
    out dx, al
    pop rdx
    pop rax

    ; Jump to Kernel in Higher-Half space
    mov rax, higher_half_entry
    mov rbx, 0xFFFF800000000000
    or rax, rbx
    jmp rax

higher_half_entry:
    ; 1. Reload GDTR with higher-half virtual address BEFORE unmapping lower half
    mov rax, gdt_descriptor64
    mov rbx, 0xFFFF800000000000
    or rax, rbx
    lgdt [rax]

    ; Reload segment selectors with higher-half GDT active
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    ; 2. NOTE: the lower-half identity mapping (PML4[0]) used to be torn
    ; down right here. Moved to Rust -- see memory::unmap_low_half_identity_map
    ; and its call site in kmain() (main.rs), right after interrupts::init()
    ; returns -- so the teardown happens once a real IDT is live instead of
    ; before one exists. Nothing between here and there is expected to touch
    ; a low address, but *if* something did, this way it can fault cleanly
    ; instead of triple-faulting with no diagnostics.

    ; 3. Enable SSE & FPU for Rust using full 64-bit register operations
    mov rax, cr0
    and rax, ~(1 << 2)  ; Clear Coprocessor Emulation (CR0.EM)
    or rax, (1 << 1)    ; Set Coprocessor Monitoring (CR0.MP)
    mov cr0, rax

    mov rax, cr4
    or rax, (3 << 9)    ; Set CR4.OSFXSR (bit 9) and CR4.OSXMMEXCPT (bit 10)
    mov cr4, rax

    ; --- Boot checkpoint 'U' ---
    push rax
    push rdx
    mov dx, 0x3f8
    mov al, 'U'
    out dx, al
    pop rdx
    pop rax

    ; 4. Align stack to satisfy System V AMD64 ABI on function entry
    sub rsp, 8

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

gdt_descriptor64:
    dw gdt_end - gdt_start - 1
    dq 0xFFFF800000000000 + gdt_start

CODE_SEG   equ gdt_code   - gdt_start
DATA_SEG   equ gdt_data   - gdt_start
CODE64_SEG equ gdt_code64 - gdt_start

; --- Pad stage2 to exactly 32KB (64 sectors) so the kernel starts at LBA 65 ---
times 32768 - ($ - $$) db 0
