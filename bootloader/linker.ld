OUTPUT_FORMAT(binary)
ENTRY(_start)
SECTIONS {
    . = 0x7c00;
    .text : {
        *(.text)
    }
    .rodata : {
        *(.rodata)
    }
    .data : {
        *(.data)
    }
    /* Fill remaining space to 512 bytes */
    /DISCARD/ : { *(*) }
}
