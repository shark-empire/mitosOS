ENTRY(_start)

SECTIONS
{
    /* Link the kernel in the Higher-Half virtual address space */
    . = 0xFFFF800000100000;

    /* Aligning sections to 4K (page boundaries) is vital for future memory 
       protection and prevents SIMD unaligned memory faults */
    .text : ALIGN(4096) {
        /* _start (boot_x86.s, .text.boot) MUST come first: it's what the
           bootloader actually jumps to (KERNEL_VIRT_LOAD_ADDR == start of
           this image). It zeroes .bss and only then `call`s kmain, which
           is what makes .bss actually zero by the time Rust runs -- kmain
           (.text.entry) must not be first, or that zeroing gets skipped. */
        KEEP(*(.text.boot))
        KEEP(*(.text.entry))
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
