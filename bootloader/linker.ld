OUTPUT_FORMAT(binary)
ENTRY(_start)

SECTIONS {
    . = 0x7c00;

    .text : {
        *boot.o(.text)   /* boot.s must be first — BIOS jumps to byte 0 */
        *(.text)
    }

    .rodata : {
        *(.rodata)
    }

    .data : {
        *(.data)
    }

    .bss : {
        _bss_start = .;
        *(.bss)
        *(COMMON)
        _bss_end = .;
    }

    /* Drops anything not placed above (debug info, comments).
       Does NOT pad to 512 bytes — that's handled in boot.s itself. */
    /DISCARD/ : { *(*) }
}
