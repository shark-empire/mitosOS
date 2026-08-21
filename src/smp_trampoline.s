; src/smp_trampoline.s -- x86_64 SMP AP bootstrap trampoline.
;
; Assembled as a *flat* binary (`-f bin`, see build.rs's smp_trampoline
; rule) and copied verbatim by hal::smp to a fixed low physical page
; (TRAMPOLINE_PHYS = 0x8000 -- see hal/smp.rs) before each AP is sent a
; Startup IPI whose vector encodes that same page
; (vector = TRAMPOLINE_PHYS >> 12).
;
; Per the SIPI architectural contract, the target core starts
; executing with CS.base == TRAMPOLINE_PHYS and IP == 0, in 16-bit
; real mode. `ORG TRAMPOLINE_PHYS` below (not `ORG 0`) makes every
; label in this file equal to its true physical/linear address --
; correct for the far jumps below (a far jump's offset operand becomes
; the literal new IP/EIP, it is never added to any prior segment base)
; and, since the 16-bit prologue explicitly sets DS=SS=0 (flat, base
; 0) before doing anything else, also correct for the `lgdt`/
; `mov ..., [params_*]` memory operands. Nothing in this file does a
; *near* jump/call while in 16-bit mode -- the only 16-bit control
; transfer at all is the single far jump into 32-bit code below, which
; is exactly what makes ORG-as-absolute safe here (a near jump would
; instead want offsets relative to TRAMPOLINE_PHYS, i.e. plain `ORG
; 0` -- the two conventions don't mix within the same jump).
;
; hal::smp writes four 8-byte parameters into this same page (the
; `params_*` labels at the very end) before each Startup IPI:
;   - the physical CR3 to load -- the BSP's own *live* page table,
;     see hal/smp.rs's module doc comment for why that's both
;     sufficient and necessary here
;   - this AP's kernel stack top (a virtual/high-half address, valid
;     the instant CR3 above is loaded)
;   - the Rust entry point to jump to (also a high-half address)
;   - this AP's logical cpu_index (hal::smp's own bookkeeping index,
;     passed through in rdi as rust_ap_entry's first argument)

BITS 16
ORG 0x8000

trampoline_start:
    cli
    cld

    ; Flat real-mode addressing: DS=SS=0 so every `[label]` operand
    ; below (computed by NASM as an absolute address, per this file's
    ; ORG) resolves to the matching linear address directly.
    xor ax, ax
    mov ds, ax
    mov ss, ax
    mov sp, 0x7C00              ; scratch stack; unused low memory, well below this page

    lgdt [gdt32_ptr]

    mov eax, cr0
    or eax, 1                   ; CR0.PE
    mov cr0, eax

    jmp dword 0x08:pmode_entry  ; far jump: flushes the prefetch queue and loads CS = flat 32-bit code

BITS 32
pmode_entry:
    mov ax, 0x10                ; flat 32-bit data selector
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov eax, cr4
    or eax, 1 << 5               ; CR4.PAE (required before long mode)
    mov cr4, eax

    mov eax, [params_cr3]        ; low 32 bits are enough -- physical addresses here never exceed 4GB
    mov cr3, eax

    mov ecx, 0xC0000080          ; IA32_EFER
    rdmsr
    or eax, 1 << 8                ; EFER.LME
    wrmsr

    mov eax, cr0
    or eax, 1 << 31               ; CR0.PG -- from this instruction on, addresses go through params_cr3's
    mov cr0, eax                  ; page tables (see this file's header comment for why TRAMPOLINE_PHYS
                                   ; must still be identity-mapped there)

    jmp 0x18:lmode_entry          ; far jump into the 64-bit (L=1) code segment -- this is what actually
                                   ; activates long mode

BITS 64
lmode_entry:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov rsp, [params_stack_top]
    mov rdi, [params_cpu_index]   ; SysV ABI: 1st integer argument
    mov rax, [params_entry]
    jmp rax

; --- Temporary flat GDT, used only to reach long mode. gdt.rs's own
; per-CPU GDT (gdt::init_ap) takes over inside rust_ap_entry, before
; this core touches anything else. ---
align 8
gdt32:
    dq 0x0000000000000000     ; 0x00 null
    dq 0x00CF9A000000FFFF     ; 0x08 32-bit flat code (G=1,D=1)
    dq 0x00CF92000000FFFF     ; 0x10 32-bit flat data (G=1,D=1)
    dq 0x00AF9A000000FFFF     ; 0x18 64-bit flat code (G=1,L=1,D=0)
    dq 0x00CF92000000FFFF     ; 0x20 64-bit "data" -- base/limit/G/D are meaningless for data
                               ;      descriptors in long mode, so this is deliberately the same
                               ;      value as 0x10 (gdt.rs's own kernel_data uses the same trick)
gdt32_end:

gdt32_ptr:
    dw gdt32_end - gdt32 - 1
    dd gdt32

; --- Parameter block. hal/smp.rs's PARAM_* constants hardcode these
; same offsets (0x1F0 from TRAMPOLINE_PHYS); the padding below
; guarantees that regardless of how the code above changes, the two
; sides never need a build-time symbol map to stay in sync. (If the
; code above ever grows past 0x1F0 bytes, this `times` fails to
; assemble with a negative-count error, rather than silently
; overlapping the parameters.) ---
times (0x1F0 - ($ - $$)) db 0

params_cr3:        dq 0
params_stack_top:  dq 0
params_entry:      dq 0
params_cpu_index:  dq 0
