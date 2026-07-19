//! Interrupt and Exception Management engine for mitosOS.
//! Abstracts the x86_64 Interrupt Descriptor Table (IDT) and the 
//! aarch64 Exception Vector Table behind a unified system interface.

#[cfg(target_arch = "aarch64")]
mod imp {
    /// Initializes the aarch64 Exception Vector Table.
    /// Sets the Vector Base Address Register (VBAR_EL1) to point to our handlers.
    pub unsafe fn init() {
        extern "C" {
            // This will point to our upcoming assembly vector table definition
            static exception_vector_table: u8;
        }

        unsafe {
            let table_ptr = &exception_vector_table as *const u8 as usize;
            // Load the address of the table straight into the VBAR_EL1 system register
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
    /// Alignment requirement for the x86_64 IDT structure
    #[repr(align(16))]
    struct InterruptDescriptorTable {
        // We will define the 256 interrupt gate descriptors here next
        entries: [u64; 256], 
    }

    static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable { entries: [0; 256] };

    /// Initializes the x86_64 Interrupt Descriptor Table (IDT)
    /// and issues the `lidt` instruction to alert the CPU.
    pub unsafe fn init() {
        // We will fill out the entries (e.g., mapping Page Faults, double faults, and UART) next.
        
        #[repr(C, packed)]
        struct IdtPointer {
            limit: u16,
            base: usize,
        }

        unsafe {
            let idt_ptr = IdtPointer {
                limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
                base: &IDT as *const _ as usize,
            };

            // Load the IDT into the processor
            core::arch::asm!(
                "lidt [{}]",
                in(reg) &idt_ptr,
                options(readonly, nostack, preserves_flags)
            );
        }
    }
}

/// Exposed unified initialization hook called directly by `main.rs`
/// right after memory management systems are brought online.
pub fn init() {
    unsafe {
        imp::init();
    }
}
