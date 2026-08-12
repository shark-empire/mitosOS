/* linker_x86.ld */

ENTRY(_start)

KERNEL_VMA      = 0xffffffff80000000;
KERNEL_LMA_BASE = 0x00100000; /* 1MiB: conventional Multiboot load point */

PHDRS
{
    text   PT_LOAD FLAGS(5); /* Read + Execute (4 + 1) */
    rodata PT_LOAD FLAGS(4); /* Read-Only (4) */
    data   PT_LOAD FLAGS(6); /* Read + Write (4 + 2) */
}

SECTIONS
{
    . = KERNEL_LMA_BASE + KERNEL_VMA;

    /* --- TEXT SEGMENT (Read + Execute) --- */
    .multiboot2_header : AT(ADDR(.multiboot2_header) - KERNEL_VMA) {
        KEEP(*(.multiboot2_header))
    } :text

    .text ALIGN(4096) : AT(ADDR(.text) - KERNEL_VMA) {
        KEEP(*(.text.boot_multiboot2))
        KEEP(*(.text.boot))
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :text

    /* Force full 4KiB page alignment before switching PHDR permissions */
    . = ALIGN(4096);

    /* --- RODATA SEGMENT (Read Only) --- */
    .rodata ALIGN(4096) : AT(ADDR(.rodata) - KERNEL_VMA) {
        *(.rodata .rodata.*)
    } :rodata

    /* Force full 4KiB page alignment before switching PHDR permissions */
    . = ALIGN(4096);

    /* --- DATA SEGMENT (Read + Write) --- */

    /* Limine requests must be writable so Limine can write responses back */
    .limine_requests_start ALIGN(8) : AT(ADDR(.limine_requests_start) - KERNEL_VMA) {
        KEEP(*(.limine_requests_start))
    } :data

    .limine_requests ALIGN(8) : AT(ADDR(.limine_requests) - KERNEL_VMA) {
        KEEP(*(.limine_requests))
    } :data

    .limine_requests_end ALIGN(8) : AT(ADDR(.limine_requests_end) - KERNEL_VMA) {
        KEEP(*(.limine_requests_end))
    } :data

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
