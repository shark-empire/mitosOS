ENTRY(kmain)

SECTIONS
{
    /* Link the kernel in the Higher-Half virtual address space */
    . = 0xFFFF800000100000;

    /* Aligning sections to 4K (page boundaries) is vital for future memory 
       protection and prevents SIMD unaligned memory faults */
    .text : ALIGN(4096) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    _kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
