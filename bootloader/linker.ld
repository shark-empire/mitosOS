OUTPUT_FORMAT(binary)
ENTRY(_start)

SECTIONS {
    . = 0x8000;

    .text : {
        *stage2.o(.text)
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

    /DISCARD/ : { *(*) }
}
