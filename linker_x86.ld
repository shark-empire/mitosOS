/* Set the entry point to the _start symbol defined in boot_x86.s */
ENTRY(_start)

SECTIONS
{
    /* Start at 0x8000 (typical for simple kernels) */
    . = 0x8000;

    .text : {
        /* Keep the assembly stub at the absolute beginning of the binary */
        KEEP(*(.text.boot)) 
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
