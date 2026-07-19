[bits 16]

section .text
global _start

KERNEL_TEMP_SEGMENT equ 0x1000     ; 0x1000:0x0000 = physical 0x10000, temp buffer
KERNEL_TEMP_OFFSET  equ 0x0000
KERNEL_SECTOR_COUNT equ 256        ; 128KB placeholder — tune once kernel is built
KERNEL_START_LBA    equ 65         ; sector 0=stage1, 1-64=stage2, 65+=kernel
                                    ; MUST match build.sh's disk.img layout
KERNEL_LOAD_ADDR    equ 0x100000   ; final home: 1MB

_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    ; Load the kernel now, while BIOS int 13h is still available —
    ; it stops working the moment we enter protected mode below
    mov si, kernel_dap
    mov ah, 0x42
    mov dl, [0x0500]        ; boot drive, stashed by stage1 at this fixed address
    int 0x13
    jc disk_error

    ; Enable A20 — without this, memory above 1MB wraps and
    ; both the kernel relocation copy and the kernel itself
    ; would silently read/write the wrong address
    in al, 0x92
    or al, 2
    out 0x92, al

    lgdt [gdt_descriptor]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    jmp CODE_SEG:protected_mode_start

disk_error:
    cli
    hlt
    jmp $

kernel_dap:
    db 0x10
    db 0
    dw KERNEL_SECTOR_COUNT
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

    ; Move the kernel from the temp real-mode buffer to its real home.
    ; Only possible now — flat 32-bit addressing can reach 1MB, real
    ; mode segment:offset addressing couldn't land exactly there.
    mov esi, (KERNEL_TEMP_SEGMENT * 16) + KERNEL_TEMP_OFFSET
    mov edi, KERNEL_LOAD_ADDR
    mov ecx, (KERNEL_SECTOR_COUNT * 512) / 4
    cld
    rep movsd

    ; --- Build minimal page tables: identity-map the first 2MB ---
    ; Long mode cannot be entered with paging off — this is required,
    ; not optional. One 2MB page is enough: it covers our stack (0x90000),
    ; these page tables themselves, and the kernel at 0x100000.
    mov edi, 0x1000          ; PML4 table
    mov ecx, 3072            ; zero 3 pages (PML4+PDPT+PD) = 12KB = 3072 dwords
    xor eax, eax
    rep stosd

    mov dword [0x1000], 0x2003   ; PML4[0] -> PDPT at 0x2000, present+writable
    mov dword [0x2000], 0x3003   ; PDPT[0] -> PD at 0x3000, present+writable
    mov dword [0x3000], 0x83     ; PD[0] = 2MB page at addr 0, present+writable+PS

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

    ; Still executing 32-bit code (compatibility submode) until CS
    ; is reloaded with a segment that has the L-bit set — same reason
    ; the earlier real->protected jump was mandatory, one level up
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
    db 10011010b     ; same access byte — present, ring0, code, exec/read
    db 10101111b     ; flags: G=1, D=0(required when L=1), L=1, limit high=1111
    db 0x0
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd gdt_start

CODE_SEG   equ gdt_code   - gdt_start
DATA_SEG   equ gdt_data   - gdt_start
CODE64_SEG equ gdt_code64 - gdt_start
