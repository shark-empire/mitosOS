OUTPUT_FORMAT(binary)
ENTRY(_start)

SECTIONS {
    /* Link the kernel in the Higher-Half virtual address space */
    . = 0xFFFF800000100000;

    .text : ALIGN(4K) {
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
    }

    .bss : ALIGN(4K) {
        *(.bss .bss.*)
        *(COMMON)
    }

    _kernel_end = .;



    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
