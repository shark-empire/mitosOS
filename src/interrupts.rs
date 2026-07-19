//! Interrupt and Exception Management engine for mitosOS.
//! Abstracts the x86_64 Interrupt Descriptor Table (IDT) and the 
//! aarch64 Exception Vector Table behind a unified system interface.

#[cfg(target_arch = "aarch64")]
mod imp {
    pub unsafe fn init() {
       unsafe extern "C" {
            static exception_vector_table: u8;
        }
        unsafe {
            let table_ptr = &exception_vector_table as *const u8 as usize;
            core::arch::asm!(
                "msr vbar_el1, {}",
                in(reg) table_ptr,
                options(nostack, nomem)
            );
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    /// Definition of a standard x86_64 IDT Gate Descriptor (16 bytes)
    #[derive(Copy, Clone)]
    #[repr(C, packed)]
    struct IdtEntry {
        pointer_low: u16,
        gdt_selector: u16,
        options: u16,
        pointer_middle: u16,
        pointer_high: u32,
        reserved: u32,
    }

    impl IdtEntry {
        const fn missing() -> Self {
            Self {
                pointer_low: 0,
                gdt_selector: 0,
                options: 0,
                pointer_middle: 0,
                pointer_high: 0,
                reserved: 0,
            }
        }

        fn set_handler(&mut self, handler_addr: usize) {
            self.pointer_low = handler_addr as u16;
            self.gdt_selector = 0x08; // Kernel code segment offset in GDT
            self.options = 0x8E00;    // Present, Ring 0, 64-bit Interrupt Gate Type
            self.pointer_middle = (handler_addr >> 16) as u16;
            self.pointer_high = (handler_addr >> 32) as u32;
            self.reserved = 0;
        }
    }

    #[repr(align(16))]
    struct InterruptDescriptorTable {
        entries: [IdtEntry; 256], 
    }

    static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable { 
        entries: [IdtEntry::missing(); 256] 
    };

    // Low-level assembly entry stubs declared below
   unsafe extern "C" {
        fn exception_handler_stub();
        fn uart_handler_stub();
    }

    /// Helper for legacy 8259 PIC port communication
    unsafe fn pic_outb(port: u16, value: u8) {
        core::arch::asm!(
            "out dx, al", in("dx") port, in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    /// Remaps the legacy PIC vectors out of the way of CPU Exceptions.
    /// By default, hardware interrupts map to 0x08-0x0F, conflicting with Double Faults.
    /// We remap Master PIC to 0x20-0x27 and Slave PIC to 0x28-0x2F.
    unsafe fn remap_pic() {
        pic_outb(0x20, 0x11); // Initialization command
        pic_outb(0xA0, 0x11);

        pic_outb(0x21, 0x20); // Master vector offset (0x20)
        pic_outb(0xA1, 0x28); // Slave vector offset (0x28)

        pic_outb(0x21, 0x04); // Tell Master PIC there is a slave at IRQ2
        pic_outb(0xA1, 0x02); // Tell Slave PIC its cascade identity

        pic_outb(0x21, 0x01); // Enable 8086 mode
        pic_outb(0xA1, 0x01);

        // Mask all lines except IRQ4 (COM1 Serial Port) on the Master PIC.
        // IRQ4 is bit 4. Clear bit means enabled -> 0b1110_1111 = 0xEF
        pic_outb(0x21, 0xEF);
        pic_outb(0xA1, 0xFF); // Mask all interrupts on Slave PIC
    }

    pub unsafe fn init() {
        // 1. Remap routing hardware controllers
        remap_pic();

        // 2. Map standard CPU exceptions (e.g., Vector 3: Breakpoint Exception)
        IDT.entries[3].set_handler(exception_handler_stub as usize);

        // 3. Map COM1 hardware line (Vector 0x20 + IRQ 4 = 0x24)
        IDT.entries[0x24].set_handler(uart_handler_stub as usize);

        // 4. Load the table descriptor pointer into the CPU
        #[repr(C, packed)]
        struct IdtPointer {
            limit: u16,
            base: usize,
        }

        let idt_ptr = IdtPointer {
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
            base: &IDT as *const _ as usize,
        };

        core::arch::asm!(
            "lidt [{}]",
            in(reg) &idt_ptr,
            options(readonly, nostack, preserves_flags)
        );

        // 5. Unmask CPU execution execution stream flags to allow external IRQs
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }

    /// The safe Rust landing target for raw hardware serial interrupts
    #[unsafe(no_mangle)]
    pub extern "C" fn raw_uart_interrupt_handler() {
        // Acknowledge the interrupt to the PIC immediately by sending 
        // an End of Interrupt (EOI) command byte (0x20) to the control register.
        unsafe { pic_outb(0x20, 0x20); }
    }

    /// Safe Rust landing target for general debugging break exceptions
    #[unsafe(no_mangle)]
    pub extern "C" fn generic_exception_handler() {
        // Fallback catch loop for unexpected structural crashes
    }
}

// Low-Level x86_64 Assembly Wrappers:
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".global exception_handler_stub",
    ".global uart_handler_stub",
    
    "exception_handler_stub:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call generic_exception_handler",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rax",
    "  iretq",

    "uart_handler_stub:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call raw_uart_interrupt_handler",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rax",
    "  iretq"
);

// Low-Level AArch64 Exception Vector Table Wrapper:
// The table must be aligned to a 2048-byte boundary (.balign 2048).
// It contains 4 target context configurations, containing 4 vector entries each.
// Each individual entry must occupy exactly 128 bytes (.balign 128).
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".section .text",
    ".balign 2048",
    ".global exception_vector_table",
    
    "exception_vector_table:",
    
    // --- 1. Current EL with SP_EL0 ---
    "b .",  ".balign 128", // Synchronous
    "b .",  ".balign 128", // IRQ / vIRQ
    "b .",  ".balign 128", // FIQ / vFIQ
    "b .",  ".balign 128", // SError / vSError

    // --- 2. Current EL with SP_ELx ---
    "b .",  ".balign 128", // Synchronous
    "b .",  ".balign 128", // IRQ / vIRQ
    "b .",  ".balign 128", // FIQ / vFIQ
    "b .",  ".balign 128", // SError / vSError

    // --- 3. Lower EL using AArch64 ---
    "b .",  ".balign 128", // Synchronous
    "b .",  ".balign 128", // IRQ / vIRQ
    "b .",  ".balign 128", // FIQ / vFIQ
    "b .",  ".balign 128", // SError / vSError

    // --- 4. Lower EL using AArch32 ---
    "b .",  ".balign 128", // Synchronous
    "b .",  ".balign 128", // IRQ / vIRQ
    "b .",  ".balign 128", // FIQ / vFIQ
    "b .",  ".balign 128"  // SError / vSError
);

/// Exposed unified initialization hook called directly by `main.rs`
pub fn init() {
    unsafe {
        imp::init();
    }
}
