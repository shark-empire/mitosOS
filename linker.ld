ENTRY(_start)

SECTIONS
{
    /* The Raspberry Pi's GPU firmware always jumps to this fixed address
       in AArch64 mode after loading kernel8.img. */
    . = 0x80000;

    .text : {
        KEEP(*(.text._start))
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
