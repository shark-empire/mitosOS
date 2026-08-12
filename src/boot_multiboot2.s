; src/boot_multiboot2.s
;
; Multiboot2 header + 32-bit protected-mode entry trampoline.
;
; This exists so the SAME kernel image that boots via the native
; Limine protocol (src/limine.rs, and boot_x86.s' `_start`, which
; Limine calls directly in 64-bit long mode with paging already set
; up) can ALSO be booted by any Multiboot2-compliant loader: GRUB,
; Limine itself when a boot entry is configured with
; `protocol: multiboot2` (see limine.conf), QEMU's `-kernel`, etc.
; Multiboot2 hands off in 32-bit protected mode with NO paging and NO
; long mode -- the loader cannot do that part for us, so this file
; builds minimal page tables, enables PAE + long mode + paging, and
; jumps into 64-bit code -- then hands off into the SAME shared 64-bit
; entry tail (`_start`, in boot_x86.s) that the Limine path uses, via
; the register relay `_start` already implements (RDI/RSI are
; forwarded, untouched, from entry through to `call kmain` -- see the
; comment at the top of boot_x86.s).
;
; --- Why this code never uses its own linked (virtual) address ---
; linker_x86.ld links this whole image, including this file, at a
; *high* virtual address (0xffffffff80000000+, required by Limine)
; while placing it (AT()) at a *low* physical address (1MiB+, required
; by Multiboot2/GRUB). That's fine for code that only ever runs after
; paging is enabled -- but every instruction below runs *before*
; paging exists, while the CPU is still fetching from the low physical
; address. If this code referenced its own symbols normally (e.g.
; `lgdt [gdt64_descriptor]`), it would use their *linked* (high,
; ~0xffffffff80xxxxxx) values, which are not valid physical addresses
; and nothing is mapped there yet -> instant crash. So, until the far
; jump into long mode below (at which point the higher-half mapping
; this code itself builds is already live, making high addresses
; meaningful), this file avoids that entirely: control flow uses only
; local/relative jumps (position-independent regardless of load
; address), and the page tables and this trampoline's own tiny GDT are
; built at fixed, hand-picked low physical scratch addresses (plain
; numeric constants, not linked symbols).
;
; This is also why the Multiboot2 header below carries an explicit
; "entry address" tag rather than relying on the ELF entry point
; (e_entry): e_entry would be this same unusable high linked address.
; The tag instead gives the loader _start_multiboot2's actual (low)
; physical load address directly, computed by the linker from
; `_start_multiboot2 - KERNEL_VMA_OFFSET`.

[bits 32]

KERNEL_VMA_OFFSET equ 0xffffffff80000000  ; MUST match linker_x86.ld's KERNEL_VMA

; ============================= Multiboot2 header =============================
; Spec: must be 8-byte aligned and fully contained in the first 32KiB
; of the file. Being the very first linker section (linker_x86.ld)
; guarantees both.
MB2_MAGIC      equ 0xe85250d6
MB2_ARCH_I386  equ 0    ; protected-mode i386 -- correct even for a 64-bit
                          ; kernel; the header only distinguishes i386 vs
                          ; MIPS32, not word width.

section .multiboot2_header
align 8
mb2_header_start:
    dd MB2_MAGIC
    dd MB2_ARCH_I386
    dd mb2_header_end - mb2_header_start
    dd -(MB2_MAGIC + MB2_ARCH_I386 + (mb2_header_end - mb2_header_start)) & 0xffffffff

    align 8
mb2_tag_entry_address:
    dw 3                                                    ; type = entry address
    dw 0                                                    ; flags
    dd mb2_tag_entry_address_end - mb2_tag_entry_address     ; size
    dd (_start_multiboot2 - KERNEL_VMA_OFFSET)

mb2_tag_entry_address_end:

    align 8                     ; mandatory end tag
    dw 0
    dw 0
    dd 8
mb2_header_end:

; ============================= 32-bit entry =============================
section .text.boot_multiboot2
global _start_multiboot2
extern _start

; Scratch physical addresses for this trampoline's own page tables and
; GDT. Chosen well below the kernel's 1MiB load point.
PML4_ADDR     equ 0x1000
ID_PDPT_ADDR  equ 0x2000
ID_PD_ADDR    equ 0x3000
HH_PDPT_ADDR  equ 0x4000
HH_PD_ADDR    equ 0x5000
GDT_ADDR      equ 0x6000   ; 3 x 8-byte descriptors = 24 bytes
GDTR_ADDR     equ 0x6020   ; 6-byte pseudo-descriptor (2-byte limit + 4-byte base)
TMP_STACK_TOP equ 0x8000   ; one scratch page, 0x7000-0x8000, stack grows down;
                             ; see the comment above the `mov esp,` that uses this

; PML4[511] / PDPT[510] is where 0xffffffff80000000 (KERNEL_VMA_OFFSET,
; the kernel's own higher-half base) lives in canonical 4-level paging:
; bits 47:39 select the PML4 entry, bits 38:30 the PDPT entry. Masking
; 0xffffffff80000000 to its low 48 (canonical) bits gives
; 0x0000ffff80000000; its top 9 bits are all 1 (511), and the next 9
; are 0b111111110 (510). A single 1GiB PDPT entry, mapped with 2MiB
; pages, covers the entire top-2GiB window from its very start --
; comfortably more than a kernel image needs.
HH_PML4_IDX  equ 511
HH_PDPT_IDX  equ 510

; PML4[256] is a *second* alias for the exact same physical [0, 1GiB)
; this trampoline identity-maps at PML4[0] -- see the comment above
; the instruction that installs it, further down, for why.
ALIAS_PML4_IDX equ 256

_start_multiboot2:
    cli

    mov dx, 0x3f8              ; checkpoint 'Q': multiboot2 trampoline reached
    mov al, 'Q'
    out dx, al

    ; Per the Multiboot2 spec, only EAX (magic) and EBX (info struct
    ; physical pointer) are guaranteed meaningful at entry; ESP is
    ; unspecified, so nothing here uses the stack (no push/pop/call)
    ; until this trampoline sets one up itself, below.
    cmp eax, 0x36d76289
    je .magic_ok

    mov dx, 0x3f8               ; checkpoint '!': bad magic
    mov al, '!'
    out dx, al
.hang:
    cli
    hlt
    jmp .hang

.magic_ok:
    ; Stash the info pointer in ESI -- untouched by every step below,
    ; including the zero loop (which uses EDI), until the very end,
    ; where it is relayed into the registers `_start` forwards through
    ; to `call kmain`.
    mov esi, ebx

    ; Zero 6 scratch pages (0x1000-0x7000): PML4, identity PDPT+PD,
    ; higher-half PDPT+PD, GDT+GDTR.
    mov edi, PML4_ADDR
    mov ecx, 6 * 1024          ; 6 pages * 4096 bytes / 4 bytes-per-stosd
    xor eax, eax
    cld
    rep stosd

    ; PML4[0] -> identity PDPT (temporary; needed only so the
    ; instruction fetch at the exact physical address executing right
    ; now stays valid the instant CR0.PG turns on).
    mov dword [PML4_ADDR], ID_PDPT_ADDR | 3

    ; PML4[256] -> the *same* identity PDPT, a second time. This isn't
    ; needed for the mode transition below (PML4[0] alone covers
    ; that) -- it exists so memory::phys_to_virt's default offset
    ; (0xFFFF800000000000, i.e. PML4 index 256) is already correct for
    ; a Multiboot2 boot, with nothing in kmain needing to call
    ; memory::set_hhdm_offset on this path at all: PML4[0] is torn
    ; down early in kmain as a hardening step
    ; (memory::unmap_low_half_identity_map), but this second reference
    ; to the identical PDPT/PD, sitting in a PML4 slot nothing ever
    ; tears down, keeps physical [0, 1GiB) reachable at that offset for
    ; the rest of boot.
    mov dword [PML4_ADDR + ALIAS_PML4_IDX * 8], ID_PDPT_ADDR | 3

    ; PML4[511] -> higher-half PDPT (the kernel's own mapping)
    mov dword [PML4_ADDR + HH_PML4_IDX * 8], HH_PDPT_ADDR | 3

    ; identity PDPT[0] -> identity PD
    mov dword [ID_PDPT_ADDR], ID_PD_ADDR | 3

    ; higher-half PDPT[510] -> higher-half PD
    mov dword [HH_PDPT_ADDR + HH_PDPT_IDX * 8], HH_PD_ADDR | 3

    ; Both PDs get the identical mapping: physical [0, 1GiB) via 512 x
    ; 2MiB present+writable huge pages. This is enough to cover both
    ; this trampoline's own (low, identity-mapped) execution and the
    ; whole kernel image (mapped at the high alias, at KERNEL_VMA_OFFSET
    ; plus its physical offset from 0) -- the kernel is nowhere near
    ; 1GiB in size.
    mov edi, ID_PD_ADDR
    mov eax, 0x83                ; present + writable + page-size(2MiB)
    mov ecx, 512
.map_identity_loop:
    mov [edi], eax
    mov dword [edi + 4], 0
    add eax, 0x200000
    add edi, 8
    loop .map_identity_loop

    mov edi, HH_PD_ADDR
    mov eax, 0x83
    mov ecx, 512
.map_hh_loop:
    mov [edi], eax
    mov dword [edi + 4], 0
    add eax, 0x200000
    add edi, 8
    loop .map_hh_loop

    ; Build this trampoline's own minimal GDT (null, 64-bit code,
    ; 64-bit-usable data) directly into scratch memory at a fixed
    ; address -- NOT as linked `dq` data, per the file header comment.
    ; Byte values match a standard flat 64-bit code/data descriptor
    ; pair (present, non-conforming code / writable data, long mode).
    mov dword [GDT_ADDR + 8],  0x0000ffff   ; code64: limit=0xffff, base_lo=0
    mov dword [GDT_ADDR + 12], 0x00af9a00   ; code64: access=9a, flags/limhi=af
    mov dword [GDT_ADDR + 16], 0x0000ffff   ; data:   limit=0xffff, base_lo=0
    mov dword [GDT_ADDR + 20], 0x00cf9200   ; data:   access=92, flags/limhi=cf
    mov word  [GDTR_ADDR], 0x17             ; limit = 3*8 - 1
    mov dword [GDTR_ADDR + 2], GDT_ADDR     ; base

CODE64_SEG equ 8
DATA64_SEG equ 16

    mov dx, 0x3f8                ; checkpoint 'G': page tables + GDT built
    mov al, 'G'
    out dx, al

    mov eax, PML4_ADDR
    mov cr3, eax

    mov eax, cr4
    or eax, (1 << 5)              ; PAE
    mov cr4, eax

    mov ecx, 0xC0000080           ; EFER
    rdmsr
    or eax, (1 << 8)               ; LME
    or eax, (1 << 11)              ; NXE
    wrmsr

    mov eax, cr0
    or eax, (1 << 31) | 1          ; PG | PE
    mov cr0, eax

    mov dx, 0x3f8                 ; checkpoint 'X': paging enabled
    mov al, 'X'
    out dx, al

    lgdt [GDTR_ADDR]
    jmp CODE64_SEG:(.long_mode_entry - KERNEL_VMA_OFFSET)



[bits 64]
.long_mode_entry:
    mov ax, DATA64_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov dx, 0x3f8                 ; checkpoint 'L': long mode entered
    mov al, 'L'
    out dx, al

    ; _start (boot_x86.s) does not set up its own stack (`lea rsp, [rel
    ; stack_top]`) until partway through -- its first several
    ; instructions, including a diagnostic `call print_hex64`, run on
    ; whatever RSP it was entered with. Limine guarantees that's a
    ; valid bootloader-provided stack (PROTOCOL.md, "Machine State at
    ; Entry"); Multiboot2 explicitly does not define ESP at all. So,
    ; unlike everything above, this trampoline must hand off with a
    ; usable (if tiny and temporary -- a handful of pushes at most
    ; before _start replaces it) stack of its own: one more identity-
    ; mapped scratch page, used the same way PML4_ADDR etc. are.
    mov esp, TMP_STACK_TOP

    ; Relay (multiboot2 info ptr, magic) into the same two registers
    ; `_start` (boot_x86.s) already forwards untouched through to
    ; `call kmain` -- RDI and RSI respectively. ESI has held the info
    ; pointer since .magic_ok; nothing since then has touched it. This
    ; jump is the first point where using a normal linked symbol
    ; (`_start`'s real, high address) is safe: paging and the
    ; higher-half mapping built above are both live now.
    mov edi, esi                  ; edi = info ptr (zero-extends to rdi)
    mov esi, 0x36d76289           ; esi = magic (zero-extends to rsi)

    ; Force an absolute jump so we actually transition to the high VMA
    mov rax, strict qword _start
    jmp rax

