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

    /* Real end of the kernel's own image (code+rodata+data+bss) --
       everything from 0x100000 up to here is the kernel itself and must
       never be handed out by the physical frame allocator. See
       memory::protect_boot_memory, called from main.rs with this address
       instead of a hardcoded placeholder. */
    _kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
