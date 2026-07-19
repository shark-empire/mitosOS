OUTPUT_FORMAT(binary)
ENTRY(_start)

SECTIONS
{
    /* Must match KERNEL_LOAD_ADDR in bootloader/src/stage2.s exactly —
       stage2 copies the kernel here and jumps straight to this address */
    . = 0x100000;

    .text : {
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
