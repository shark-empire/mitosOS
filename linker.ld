ENTRY(_start)

SECTIONS
{
    /* We remove the fixed address (0x80000). The bootloader (Limine) 
       will now decide where to load us, making us portable. */
    . = 0xffffffff80000000;

    /* Limine requires this section to find your kernel's configuration */
    .requests : {
        KEEP(*(.requests))
    }

    .text : {
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
