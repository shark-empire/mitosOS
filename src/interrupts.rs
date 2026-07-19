//! Interrupt and Exception Management engine for mitosOS.
//! Abstracts the x86_64 Interrupt Descriptor Table (IDT) and the 
//! aarch64 Exception Vector Table behind a unified system interface.
use core::sync::atomic::{AtomicUsize, Ordering};

const BUFFER_SIZE: usize = 128;
static mut INPUT_BUFFER: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];
static HEAD: AtomicUsize = AtomicUsize::new(0);
static TAIL: AtomicUsize = AtomicUsize::new(0);

/// Pushes a byte into the buffer (Called by Interrupt Handlers)
pub fn enqueue_byte(byte: u8) {
    let current_tail = TAIL.load(Ordering::Relaxed); // Only producer modifies TAIL
    let next_tail = (current_tail + 1) % BUFFER_SIZE;
    
    // FIX: Use Acquire to safely sync with the consumer's updated HEAD position
    if next_tail != HEAD.load(Ordering::Acquire) {
        unsafe { 
            // FIX: Use raw pointer syntax to bypass modern static mut restriction lints
            (*&raw mut INPUT_BUFFER)[current_tail] = byte; 
        }
        // Release ordering guarantees the byte write above happens-before TAIL changes
        TAIL.store(next_tail, Ordering::Release);
    }
}

/// Pulls a byte out of the buffer (Called by the Shell)
pub fn dequeue_byte() -> Option<u8> {
    let current_head = HEAD.load(Ordering::Relaxed); // Only consumer modifies HEAD
    
    // FIX: Use Acquire to ensure written data is visible (Crucial for AArch64)
    if current_head == TAIL.load(Ordering::Acquire) {
        None // Buffer is empty
    } else {
        unsafe {
            // FIX: Use raw pointer syntax to bypass modern static mut restriction lints
            let byte = (*&raw mut INPUT_BUFFER)[current_head];
            // FIX: Replaced invalid `Ordering::Fill` with `Ordering::Release`
            HEAD.store((current_head + 1) % BUFFER_SIZE, Ordering::Release);
            Some(byte)
        }
    }
}


#[cfg(target_arch = "aarch64")]
mod imp {
    pub unsafe fn init() {
        unsafe extern "C" {
            static exception_vector_table: u8;
        }
        
        unsafe {
            // Use modern raw pointer syntax to reference the external symbol safely
            let table_ptr = &raw const exception_vector_table as usize;
            
            // Load our table into the Vector Base Address Register
            core::arch::asm!(
                "msr vbar_el1, {}",
                in(reg) table_ptr,
                options(nostack, nomem)
            );
            
            // Unmask IRQs in the Program Status Register
            core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        }
    }


#[unsafe(no_mangle)]
pub extern "C" fn handle_irq() {
    // FIX: Explicitly enforce usize to prevent raw pointer casting errors (E0606)
    let uart_dr: usize = 0x3F00_0000 + 0x20_1000; // Data Register (Offset 0x00)
    let uart_fr: usize = uart_dr + 0x18;          // Flag Register (Offset 0x18)
    let uart_icr: usize = uart_dr + 0x44;         // Interrupt Clear Register (Offset 0x44)

    unsafe {
        // FIX: Drain the hardware RX FIFO completely so we don't miss characters.
        // Bit 4 of UARTFR is RXFE (Receive FIFO Empty). Loop while it is 0 (NOT empty).
        while (core::ptr::read_volatile(uart_fr as *const u32) & (1 << 4)) == 0 {
            // 1. Read the byte from PL011 Data Register
            let byte = core::ptr::read_volatile(uart_dr as *mut u32) as u8;

            // 2. Put it in our shared lock-free queue
            super::enqueue_byte(byte);
        }

        // 3. Clear the interrupt flags after the FIFO is fully cleared
        core::ptr::write_volatile(uart_icr as *mut u32, 0x7FF);
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
       unsafe {
            core::arch::asm!(
                "out dx, al", in("dx") port, in("al") value,
                options(nomem, nostack, preserves_flags)
            );
       }
    }

    /// Remaps the legacy PIC vectors out of the way of CPU Exceptions.
    unsafe fn remap_pic() {
        unsafe {
            pic_outb(0x20, 0x11); // Initialization command
            pic_outb(0xA0, 0x11);

            pic_outb(0x21, 0x20); // Master vector offset (0x20)
            pic_outb(0xA1, 0x28); // Slave vector offset (0x28)

            pic_outb(0x21, 0x04); // Tell Master PIC there is a slave at IRQ2
            pic_outb(0xA1, 0x02); // Tell Slave PIC its cascade identity

            pic_outb(0x21, 0x01); // Enable 8086 mode
            pic_outb(0xA1, 0x01);

            // Mask all lines except IRQ4 (COM1 Serial Port) on the Master PIC.
            pic_outb(0x21, 0xEF);
            pic_outb(0xA1, 0xFF); // Mask all interrupts on Slave PIC
        }
    }

    pub unsafe fn init() {
        unsafe {
            // 1. Remap routing hardware controllers
            remap_pic();

            // 2. Map standard CPU exceptions (Cast to pointer before integer)
            IDT.entries[3].set_handler(exception_handler_stub as *const () as usize);

            // 3. Map COM1 hardware line (Cast to pointer before integer)
            IDT.entries[0x24].set_handler(uart_handler_stub as *const () as usize);

            // 4. Load the table descriptor pointer into the CPU
            #[repr(C, packed)]
            struct IdtPointer {
                limit: u16,
                base: usize,
            }

            let idt_ptr = IdtPointer {
                limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
                base: &raw const IDT as usize,
            };

            core::arch::asm!(
                "lidt [{}]",
                in(reg) &idt_ptr,
                options(readonly, nostack, preserves_flags)
            );

            // 5. Unmask CPU execution execution stream flags to allow external IRQs
            core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
        }
    }

  #[unsafe(no_mangle)]
pub extern "C" fn raw_uart_interrupt_handler() {
    const COM1_DATA: u16 = 0x3F8;
    const COM1_LSR: u16 = 0x3F8 + 5; // 0x3FD - Line Status Register

    unsafe {
        // FIX: Drain the hardware FIFO completely so the interrupt line can drop back low
        loop {
            let mut lsr: u8;
            // Read the Line Status Register
            core::arch::asm!(
                "in al, dx",
                out("al") lsr,
                in("dx") COM1_LSR,
                options(nomem, nostack, preserves_flags)
            );

            // Bit 0 is Data Ready (DR). If it's 0, there is no more data to read.
            if (lsr & 1) == 0 {
                break;
            }

            // 1. Read the byte from COM1 data port
            let mut byte: u8;
            core::arch::asm!(
                "in al, dx",
                out("al") byte,
                in("dx") COM1_DATA,
                options(nomem, nostack, preserves_flags)
            );

            // 2. Put it in our shared queue
            super::enqueue_byte(byte);
        }

        // 3. Send End-of-Interrupt to PIC only after the UART hardware line is clear
        pic_outb(0x20, 0x20);
    }
}


// ==========================================
// Low-Level x86_64 Assembly Wrappers
// ==========================================
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    r#"
    .global exception_handler_stub
    .global uart_handler_stub
    
    exception_handler_stub:
      push rax
      push rcx
      push rdx
      push rsi
      push rdi
      push r8
      push r9
      push r10
      push r11
      call generic_exception_handler
      pop r11
      pop r10
      pop r9
      pop r8
      pop rdi
      pop rsi
      pop rdx
      pop rcx
      pop rax
      iretq

    uart_handler_stub:
      push rax
      push rcx
      push rdx
      push rsi
      push rdi
      push r8
      push r9
      push r10
      push r11
      call raw_uart_interrupt_handler
      pop r11
      pop r10
      pop r9
      pop r8
      pop rdi
      pop rsi
      pop rdx
      pop rcx
      pop rax
      iretq
    "#
);

// ==========================================
// Low-Level AArch64 Exception Vector Table
// ==========================================
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
    .section .text
    .balign 2048
    .global exception_vector_table
    
    exception_vector_table:
      // --- 1. Current EL with SP_EL0 ---
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128

      // --- 2. Current EL with SP_ELx ---
      b .
      .balign 128
      
      // IRQ Handler Vector Slot:
      // Allocates 160 bytes to safely preserve all caller-saved registers (x0-x18)
      // and critically x30 (the Link Register) to prevent kernel context destruction.
      sub sp, sp, #160
      stp x0, x1, [sp, #0]
      stp x2, x3, [sp, #16]
      stp x4, x5, [sp, #32]
      stp x6, x7, [sp, #48]
      stp x8, x9, [sp, #64]
      stp x10, x11, [sp, #80]
      stp x12, x13, [sp, #96]
      stp x14, x15, [sp, #112]
      stp x16, x17, [sp, #128]
      stp x18, x30, [sp, #144]

      bl handle_irq

      ldp x0, x1, [sp, #0]
      ldp x2, x3, [sp, #16]
      ldp x4, x5, [sp, #32]
      ldp x6, x7, [sp, #48]
      ldp x8, x9, [sp, #64]
      ldp x10, x11, [sp, #80]
      ldp x12, x13, [sp, #96]
      ldp x14, x15, [sp, #112]
      ldp x16, x17, [sp, #128]
      ldp x18, x30, [sp, #144]
      add sp, sp, #160
      eret
      .balign 128
      
      b .
      .balign 128
      b .
      .balign 128

      // --- 3. Lower EL using AArch64 ---
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128

      // --- 4. Lower EL using AArch32 ---
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128
    "#
);

/// Exposed unified initialization hook called directly by `main.rs`
pub fn init() {
    unsafe {
        imp::init();
    }
}
