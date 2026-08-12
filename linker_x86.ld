/* linker_x86.ld
 *
 * Linker script for mitosOS's x86_64 kernel: one image, bootable two
 * ways -- natively via Limine (src/limine.rs), or via any Multiboot2
 * loader (GRUB, Limine itself configured with `protocol: multiboot2`,
 * QEMU's `-kernel`, ...) via src/boot_multiboot2.s. See that file's
 * header comment for the full reasoning behind the split below; the
 * short version:
 *
 * Limine mandates a non-relocatable kernel be linked in the top 2GiB
 * of the address space, at/above 0xffffffff80000000 (see "Memory
 * Layout at Entry" in limine-protocol's PROTOCOL.md) -- hence the
 * high KERNEL_VMA. Multiboot2 loaders, on the other hand, load ELF
 * PT_LOAD segments at their *physical* (LMA) address, which must be
 * ordinary, always-present low RAM -- hence AT() placing everything
 * at KERNEL_LMA_BASE (1MiB, the conventional Multiboot load point)
 * instead. Both loaders are happy with the same image: Limine ignores
 * LMA/p_paddr entirely and picks its own physical placement (see the
 * "Executable Address" feature, src/limine.rs), so the AT() split
 * below is invisible to it.
 *
 * KERNEL_VMA and KERNEL_LMA_BASE MUST match the constants of the same
 * name in src/boot_multiboot2.s (used there, via linker relocation,
 * to compute the Multiboot2 "entry address" tag). If you change one,
 * change the other.
 */

ENTRY(_start)

KERNEL_VMA      = 0xffffffff80000000;
KERNEL_LMA_BASE = 0x00100000; /* 1MiB: conventional Multiboot load point */

SECTIONS
{
    . = KERNEL_LMA_BASE + KERNEL_VMA;

      /* Must be within the first 32KiB of the file and 8-byte aligned;
       being the very first section guarantees both. */
    .multiboot2_header : AT(ADDR(.multiboot2_header) - KERNEL_VMA) {
        KEEP(*(.multiboot2_header))
    }

    .text ALIGN(4096) : AT(ADDR(.text) - KERNEL_VMA) {
        /* _start_multiboot2 (32-bit trampoline, boot_multiboot2.s)
           first, then _start (boot_x86.s' shared 64-bit tail...) */
        KEEP(*(.text.boot_multiboot2))
        KEEP(*(.text.boot))
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    /* Limine request structs (src/limine.rs) */
    .limine_requests_start ALIGN(8) : AT(ADDR(.limine_requests_start) - KERNEL_VMA) {
        KEEP(*(.limine_requests_start))
    }
    .limine_requests ALIGN(8) : AT(ADDR(.limine_requests) - KERNEL_VMA) {
        KEEP(*(.limine_requests))
    }
    .limine_requests_end ALIGN(8) : AT(ADDR(.limine_requests_end) - KERNEL_VMA) {
        KEEP(*(.limine_requests_end))
    }

    .rodata ALIGN(4096) : AT(ADDR(.rodata) - KERNEL_VMA) {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4096) : AT(ADDR(.data) - KERNEL_VMA) {
        *(.data .data.*)
    }

    .bss ALIGN(4096) (NOLOAD) : AT(ADDR(.bss) - KERNEL_VMA) {
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
