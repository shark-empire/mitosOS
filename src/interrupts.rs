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
    
    if next_tail != HEAD.load(Ordering::Acquire) {
        unsafe { 
            (*&raw mut INPUT_BUFFER)[current_tail] = byte; 
        }
        TAIL.store(next_tail, Ordering::Release);
    }
}

/// Pulls a byte out of the buffer (Called by the Shell)
pub fn dequeue_byte() -> Option<u8> {
    let current_head = HEAD.load(Ordering::Relaxed); // Only consumer modifies HEAD
    
    if current_head == TAIL.load(Ordering::Acquire) {
        None // Buffer is empty
    } else {
        unsafe {
            let byte = (*&raw mut INPUT_BUFFER)[current_head];
            HEAD.store((current_head + 1) % BUFFER_SIZE, Ordering::Release);
            Some(byte)
        }
    }
}

// ==========================================
// AArch64 Implementation Module
// ==========================================
#[cfg(target_arch = "aarch64")]
mod imp {
    pub unsafe fn init() {
        unsafe extern "C" {
            static exception_vector_table: u8;
        }
        
        unsafe {
            let table_ptr = &raw const exception_vector_table as usize;
            
            // Load our table into the Vector Base Address Register
            core::arch::asm!(
                "msr vbar_el1, {}",
                in(reg) table_ptr,
                options(nostack, nomem)
            );
            
            // Note: CPU unmasking (daifclr) was removed from here. 
            // It is now explicitly handled by enable_cpu_interrupts() in main.rs.
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn handle_irq() {
        let uart_dr: usize = 0x3F00_0000 + 0x20_1000; 
        let uart_fr: usize = uart_dr + 0x18;          
        let uart_icr: usize = uart_dr + 0x44;         

        unsafe {
            // Drain the hardware RX FIFO completely so we don't miss characters.
            while (core::ptr::read_volatile(uart_fr as *const u32) & (1 << 4)) == 0 {
                let byte = core::ptr::read_volatile(uart_dr as *mut u32) as u8;
                super::enqueue_byte(byte);
            }

            // Clear the interrupt flags after the FIFO is fully cleared
            core::ptr::write_volatile(uart_icr as *mut u32, 0x7FF);
        }
    }
}

// ==========================================
// x86_64 Implementation Module
// ==========================================
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
            self.gdt_selector = 0x08; 
            self.options = 0x8E00;    
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

    unsafe extern "C" {
        fn exception_handler_stub();
        fn uart_handler_stub();
    }

    unsafe fn pic_outb(port: u16, value: u8) {
       unsafe {
            core::arch::asm!(
                "out dx, al", in("dx") port, in("al") value,
                options(nomem, nostack, preserves_flags)
            );
       }
    }

    unsafe fn remap_pic() {
        unsafe {
            pic_outb(0x20, 0x11); 
            pic_outb(0xA0, 0x11);
            pic_outb(0x21, 0x20); 
            pic_outb(0xA1, 0x28); 
            pic_outb(0x21, 0x04); 
            pic_outb(0xA1, 0x02); 
            pic_outb(0x21, 0x01); 
            pic_outb(0xA1, 0x01); 
            pic_outb(0x21, 0xEF);
            pic_outb(0xA1, 0xFF); 
        }
    }

    pub unsafe fn init() {
        unsafe {
            remap_pic();

            IDT.entries[3].set_handler(exception_handler_stub as *const () as usize);
            IDT.entries[0x24].set_handler(uart_handler_stub as *const () as usize);

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

            // Note: CPU unmasking (sti) was removed from here. 
            // It is now explicitly handled by enable_cpu_interrupts() in main.rs.
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn raw_uart_interrupt_handler() {
        const COM1_DATA: u16 = 0x3F8;
        const COM1_LSR: u16 = 0x3F8 + 5; 

        unsafe {
            loop {
                let mut lsr: u8;
                core::arch::asm!(
                    "in al, dx",
                    out("al") lsr,
                    in("dx") COM1_LSR,
                    options(nomem, nostack, preserves_flags)
                );

                if (lsr & 1) == 0 {
                    break;
                }

                let mut byte: u8;
                core::arch::asm!(
                    "in al, dx",
                    out("al") byte,
                    in("dx") COM1_DATA,
                    options(nomem, nostack, preserves_flags)
                );

                super::enqueue_byte(byte);
            }

            pic_outb(0x20, 0x20);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn generic_exception_handler() {
        // Fallback catch loop
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

// ==========================================
// Public Interface Methods
// ==========================================

/// Exposed unified initialization hook called directly by `main.rs`
pub fn init() {
    unsafe {
        imp::init();
    }
}

/// Unmasks interrupts at the CPU level, allowing the processor to 
/// receive hardware interrupt signals. This should be called after
/// IDTs and controller configurations are safely loaded.
#[inline(always)]
pub unsafe fn enable_cpu_interrupts() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }

    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
    }
}
