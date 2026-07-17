// src/boot.s
.section .text.boot
.global _start

_start:
    // 1. Set the stack pointer (sp) to the top of our stack
    ldr x0, =stack_top
    mov sp, x0

    // 2. Branch to your Rust main function
    // We will update boot.rs to have a 'kmain' function
    bl kmain

    // 3. If it ever returns, loop forever
    b .

.section .bss
.align 16
stack_bottom:
    .skip 65536 // Allocate 64KB for the stack
stack_top:
