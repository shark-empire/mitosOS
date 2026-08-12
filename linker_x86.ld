/* linker_x86.ld
 *
 * Linker script for mitosOS's x86_64 kernel: one image, bootable two
 * ways -- natively via Limine (src/limine.rs), or via any Multiboot2
 * loader (GRUB, Limine itself configured with `protocol: multiboot2`,
 * QEMU's `-kernel`, ...) via src/boot_multiboot2.s.
 */

ENTRY(_start)

KERNEL_VMA      = 0xffffffff80000000;
KERNEL_LMA_BASE = 0x00100000; /* 1MiB: conventional Multiboot load point */

/* 
 * Define explicit Program Headers (PT_LOAD segments) with distinct permissions.
 * Flags: 1 = Execute (X), 2 = Write (W), 4 = Read (R)
 */
PHDRS
{
    text   PT_LOAD FLAGS(5); /* Read + Execute (4 + 1) */
    rodata PT_LOAD FLAGS(4); /* Read-Only (4) */
    data   PT_LOAD FLAGS(6); /* Read + Write (4 + 2) */
}

SECTIONS
{
    . = KERNEL_LMA_BASE + KERNEL_VMA;

    /* Must be within the first 32KiB of the file and 8-byte aligned */
    .multiboot2_header : AT(ADDR(.multiboot2_header) - KERNEL_VMA) {
        KEEP(*(.multiboot2_header))
    } :text

    .text ALIGN(4096) : AT(ADDR(.text) - KERNEL_VMA) {
        /* _start_multiboot2 (32-bit trampoline, boot_multiboot2.s)
           first, then _start (boot_x86.s' shared 64-bit tail...) */
        KEEP(*(.text.boot_multiboot2))
        KEEP(*(.text.boot))
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :text

    /* Limine request structs (src/limine.rs) */
    .limine_requests_start ALIGN(8) : AT(ADDR(.limine_requests_start) - KERNEL_VMA) {
        KEEP(*(.limine_requests_start))
    } :data

    .limine_requests ALIGN(8) : AT(ADDR(.limine_requests) - KERNEL_VMA) {
        KEEP(*(.limine_requests))
    } :data

    .limine_requests_end ALIGN(8) : AT(ADDR(.limine_requests_end) - KERNEL_VMA) {
        KEEP(*(.limine_requests_end))
    } :data

    .rodata ALIGN(4096) : AT(ADDR(.rodata) - KERNEL_VMA) {
        *(.rodata .rodata.*)
    } :rodata

    .data ALIGN(4096) : AT(ADDR(.data) - KERNEL_VMA) {
        *(.data .data.*)
    } :data

    .bss ALIGN(4096) (NOLOAD) : AT(ADDR(.bss) - KERNEL_VMA) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data

    _kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
