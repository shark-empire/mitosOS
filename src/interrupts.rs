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
    let current_tail = TAIL.load(Ordering::Relaxed);
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
    let current_head = HEAD.load(Ordering::Relaxed);
    
    if current_head == TAIL.load(Ordering::Acquire) {
        None
    } else {
        unsafe {
            let byte = (*&raw mut INPUT_BUFFER)[current_head];
            HEAD.store((current_head + 1) % BUFFER_SIZE, Ordering::Release);
            Some(byte)
        }
    }
}

/// Shared Cross-Architecture Scheduler Hook called by Assembly IRQ Handlers
#[unsafe(no_mangle)]
pub extern "C" fn schedule(current_sp: usize) -> usize {
    crate::task::run_schedule(current_sp)
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
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn handle_irq() {
        let uart_dr: usize = 0x3F00_0000 + 0x20_1000; 
        let uart_fr: usize = uart_dr + 0x18;          
        let uart_icr: usize = uart_dr + 0x44;         

        unsafe {
            // Drain the hardware RX FIFO completely
            while (core::ptr::read_volatile(uart_fr as *const u32) & (1 << 4)) == 0 {
                let byte = core::ptr::read_volatile(uart_dr as *mut u32) as u8;
                super::enqueue_byte(byte);
            }

            // Clear the interrupt flags
            core::ptr::write_volatile(uart_icr as *mut u32, 0x7FF);
        }

        // Re-arm the AArch64 generic timer on each tick
      unsafe{super::reload_timer()};
    }

    /// Handles a synchronous exception taken at EL1 (kernel mode) --
    /// a data/instruction abort from a bad kernel pointer, an illegal
    /// instruction, etc. Not recoverable yet (no fault-recovery logic
    /// exists), so this just reports what happened instead of the
    /// previous behavior of silently hanging (`b .`) with no diagnostics.
    #[unsafe(no_mangle)]
    pub extern "C" fn handle_el1_sync_exception(esr: u64, far: u64, elr: u64) -> ! {
        use core::fmt::Write;

        // ESR_EL1[31:26] = Exception Class (EC): what kind of trap this was.
        let ec = (esr >> 26) & 0x3F;
        let name = match ec {
            0x0E => "Illegal Execution State",
            0x15 => "SVC instruction (unexpected at EL1)",
            0x20 => "Instruction Abort (from a lower EL)",
            0x21 => "Instruction Abort (same EL)",
            0x24 => "Data Abort (from a lower EL)",
            0x25 => "Data Abort (same EL)",
            _ => "Unhandled synchronous exception",
        };

        let mut uart = unsafe { crate::uart::Uart::init() };
        let _ = writeln!(uart, "\r\n!!! AArch64 EL1 EXCEPTION: {name} (EC=0x{ec:02x}) !!!");
        let _ = writeln!(uart, "    ESR_EL1 = 0x{esr:016x}");
        let _ = writeln!(uart, "    FAR_EL1 = 0x{far:016x}  (faulting address)");
        let _ = writeln!(uart, "    ELR_EL1 = 0x{elr:016x}  (faulting instruction)");

        panic!("Unhandled AArch64 EL1 exception: {name}");
    }
}

#[cfg(target_arch = "aarch64")]
pub unsafe fn init_aarch64_timer() {
    let interval: u64 = 50_000_000; 
    unsafe {
    core::arch::asm!(
        "msr cntp_tval_el0, {0}",
        "mov x1, #1",
        "msr cntp_ctl_el0, x1", 
        in(reg) interval,
        out("x1") _,
        options(nomem, nostack)
    );
    } 
}

#[cfg(target_arch = "aarch64")]
pub unsafe fn init_gic_timer_irq() {
    const GICD_BASE: usize = 0x08000000;
    const GICC_BASE: usize = 0x08010000;
unsafe {
    let gicd_ctlr = GICD_BASE as *mut u32;
    gicd_ctlr.write_volatile(1);

    let gicd_ipriority = (GICD_BASE + 0x41e) as *mut u8;
    gicd_ipriority.write_volatile(0); 

    let gicd_isenable = (GICD_BASE + 0x104) as *mut u32;
    gicd_isenable.write_volatile(1 << 30);

    let gicc_ctlr = GICC_BASE as *mut u32;
    gicc_ctlr.write_volatile(1);

    let gicc_pmr = (GICC_BASE + 0x04) as *mut u32;
    gicc_pmr.write_volatile(0xFF);
  }
}

#[cfg(target_arch = "aarch64")]
pub unsafe fn reload_timer() {
    let interval: u64 = 50_000_000;
   unsafe {
    core::arch::asm!(
        "msr cntp_tval_el0, {0}",
        in(reg) interval,
        options(nomem, nostack)
    );
   }
}

// ==========================================
// x86_64 Implementation Module
// ==========================================
#[cfg(target_arch = "x86_64")]
mod imp {
    use core::fmt::Write;

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

        /// Selects a TSS Interrupt Stack Table entry (1-7) for this gate,
        /// so the CPU switches to that dedicated stack instead of the
        /// current RSP0 when the vector fires. `0` means "don't use an
        /// IST stack" (the default `set_handler` leaves it at). Used for
        /// the double-fault gate (src/gdt.rs sets up IST1) so a corrupted
        /// or overflowed kernel stack doesn't also crash the handler
        /// that's supposed to report it.
        fn set_ist(&mut self, index: u8) {
            self.options = (self.options & 0xFF00) | (index as u16 & 0x7);
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
        fn timer_handler_stub();
        fn syscall_handler_stub();
        fn divide_error_stub();
        fn invalid_opcode_stub();
        fn double_fault_stub();
        fn general_protection_fault_stub();
        fn page_fault_stub();
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
            pic_outb(0x21, 0xEE);
            pic_outb(0xA1, 0xFF); 
        }
    }

    unsafe fn init_pit() {
        let divisor: u16 = 11931; // ~100 Hz
        unsafe {
            pic_outb(0x43, 0x36);
            pic_outb(0x40, (divisor & 0xFF) as u8);
            pic_outb(0x40, (divisor >> 8) as u8);
        }
    }

    pub unsafe fn init() {
        unsafe {
            remap_pic();
            init_pit();

            IDT.entries[3].set_handler(exception_handler_stub as *const () as usize);
            IDT.entries[0x20].set_handler(timer_handler_stub as *const () as usize);
            IDT.entries[0x24].set_handler(uart_handler_stub as *const () as usize);
            IDT.entries[0x80].set_handler(syscall_handler_stub as *const () as usize);

            // CPU fault handlers -- these used to be entirely unhandled, which
            // meant any of them firing (a bad pointer deref, a bug in the ELF
            // loader/page-table code, etc.) triple-faulted the whole VM with
            // zero diagnostics. Now they print what happened and panic
            // cleanly instead. (Note: entry 0x80/128 is set only once above --
            // it used to be overwritten here with an incompatible handler
            // that bypassed the register-saving stub; that line is gone now.)
            IDT.entries[0].set_handler(divide_error_stub as *const () as usize);
            IDT.entries[6].set_handler(invalid_opcode_stub as *const () as usize);
            IDT.entries[8].set_handler(double_fault_stub as *const () as usize);
            // Dedicated stack for #DF -- see gdt::init(), which points TSS
            // IST1 at DOUBLE_FAULT_STACK before this runs.
            IDT.entries[8].set_ist(crate::gdt::DOUBLE_FAULT_IST_NUMBER);
            IDT.entries[13].set_handler(general_protection_fault_stub as *const () as usize);
            IDT.entries[14].set_handler(page_fault_stub as *const () as usize);



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
    pub extern "C" fn generic_exception_handler() {}

    /// Shared handler for CPU faults that previously had no IDT entry at
    /// all (divide error, invalid opcode, double fault, GPF, page fault).
    /// None of these are recoverable yet -- there's no user-mode/ring-3
    /// separation or demand paging in place, so any of these firing today
    /// means a genuine kernel bug. Print what happened and panic cleanly
    /// (via the existing `#[panic_handler]`) instead of silently
    /// triple-faulting the machine.
    ///
    /// `error_code` is the real CPU-pushed error code for vectors that have
    /// one (8, 13, 14, ...), or 0 for vectors that don't (0, 6) -- the
    /// calling stub pushes a dummy 0 so the stack layout -- and therefore
    /// this function's view of it -- is uniform either way.
    #[unsafe(no_mangle)]
    pub extern "C" fn fault_common_handler(vector: u64, error_code: u64, rip: u64) -> ! {
        let name = match vector {
            0 => "Divide Error (#DE)",
            6 => "Invalid Opcode (#UD)",
            8 => "Double Fault (#DF)",
            13 => "General Protection Fault (#GP)",
            14 => "Page Fault (#PF)",
            _ => "Unhandled Exception",
        };

        let mut uart = unsafe { crate::uart::Uart::init() };
        let _ = writeln!(uart, "\r\n!!! CPU EXCEPTION: {name} (vector {vector}) !!!");
        let _ = writeln!(uart, "    error_code   = 0x{error_code:x}");
        let _ = writeln!(uart, "    faulting rip = 0x{rip:x}");

        if vector == 14 {
            // CR2 holds the faulting virtual address for a page fault --
            // it's a separate control register, untouched by the stub's
            // GPR pushes, so it's still valid to read here.
            let cr2: usize;
            unsafe {
                core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack));
            }
            let present = error_code & 1 != 0;
            let write = error_code & (1 << 1) != 0;
            let user = error_code & (1 << 2) != 0;
            let _ = writeln!(
                uart,
                "    fault address (CR2) = 0x{cr2:x}  [{}, {}, {}]",
                if present { "protection violation" } else { "page not present" },
                if write { "write" } else { "read" },
                if user { "user-mode" } else { "supervisor" },
            );
        }

        if vector == 13 {
            let _ = writeln!(
                uart,
                "    selector index = {}  [external={}, table={}]",
                error_code >> 3,
                error_code & 1 != 0,
                if error_code & (1 << 1) != 0 { "IDT" } else { "GDT/LDT" },
            );
        }

        panic!("Unhandled CPU exception: {name}");
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
    .global timer_handler_stub
    .global syscall_handler_stub
    
    exception_handler_stub:
      push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9; push r10; push r11
      call generic_exception_handler
      pop r11; pop r10; pop r9; pop r8; pop rdi; pop rsi; pop rdx; pop rcx; pop rax
      iretq

    uart_handler_stub:
      push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9; push r10; push r11
      call raw_uart_interrupt_handler
      pop r11; pop r10; pop r9; pop r8; pop rdi; pop rsi; pop rdx; pop rcx; pop rax
      iretq

    timer_handler_stub:
      push rax; push rbx; push rcx; push rdx; push rsi; push rdi; push rbp; push r8; push r9; push r10; push r11; push r12; push r13; push r14; push r15
      mov rdi, rsp
      call schedule
      mov rsp, rax
      mov al, 0x20
      out 0x20, al
      pop r15; pop r14; pop r13; pop r12; pop r11; pop r10; pop r9; pop r8; pop rbp; pop rdi; pop rsi; pop rdx; pop rcx; pop rbx; pop rax
      iretq

    syscall_handler_stub:
      push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9; push r10; push r11
      // Pass registers: rax (syscall num) -> rdi, rbx -> rsi, rcx -> rdx, rdx -> rcx
      mov rdi, rax
      mov rsi, rbx
      mov rdx, rcx
      mov rcx, r8
      call syscall_handler
      pop r11; pop r10; pop r9; pop r8; pop rdi; pop rsi; pop rdx; pop rcx; pop rax
      iretq

    // --- CPU fault stubs -----------------------------------------------
    // fault_common_handler(vector, error_code, rip) never returns (it
    // panics), so unlike the stubs above these have no pop/iretq epilogue.
    //
    // Vectors 8/13/14 have the CPU push a real error code, so it's already
    // sitting on the stack when the stub starts. Vectors 0/6 don't get one,
    // so EXC_NOERR pushes a dummy 0 first -- that keeps [rsp+72]/[rsp+80]
    // (error_code/rip) at the same offsets either way, after the 9 GPR
    // pushes below.
    .macro EXC_NOERR name, vector
    .global \name
    \name:
        push 0
        push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9; push r10; push r11
        mov rdi, \vector
        mov rsi, [rsp + 72]
        mov rdx, [rsp + 80]
        call fault_common_handler
    .endm

    .macro EXC_ERR name, vector
    .global \name
    \name:
        push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9; push r10; push r11
        mov rdi, \vector
        mov rsi, [rsp + 72]
        mov rdx, [rsp + 80]
        call fault_common_handler
    .endm

    EXC_NOERR divide_error_stub, 0
    EXC_NOERR invalid_opcode_stub, 6
    EXC_ERR   double_fault_stub, 8
    EXC_ERR   general_protection_fault_stub, 13
    EXC_ERR   page_fault_stub, 14
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
      // =========================================================
      // 1. Current EL with SP_0 (4 vectors, 128 bytes each)
      // =========================================================
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128
      b .
      .balign 128

      // =========================================================
      // 2. Current EL with SP_x (4 vectors, 128 bytes each)
      // =========================================================

      // --- Synchronous Exception Slot (Current EL, SP_x) ---
      // A kernel-mode data abort, illegal instruction, etc. used to land
      // here and just `b .` (hang) with zero diagnostics. Dump the
      // architected syndrome/fault-address/return-address registers and
      // hand off to Rust instead -- handle_el1_sync_exception never
      // returns, so there's no need to restore anything below.
      mrs x0, esr_el1
      mrs x1, far_el1
      mrs x2, elr_el1
      bl handle_el1_sync_exception
      b .
      .balign 128
      
      // --- IRQ Handler Vector Slot (Current EL, SP_x) ---
      sub sp, sp, #272

      stp x0, x1, [sp, #0]
      stp x2, x3, [sp, #16]
      stp x4, x5, [sp, #32]
      stp x6, x7, [sp, #48]
      stp x8, x9, [sp, #64]
      stp x10, x11, [sp, #80]
      stp x12, x13, [sp, #96]
      stp x14, x15, [sp, #112]
      stp x16, x17, [sp, #128]
      stp x18, x19, [sp, #144]
      stp x20, x21, [sp, #160]
      stp x22, x23, [sp, #176]
      stp x24, x25, [sp, #192]
      stp x26, x27, [sp, #208]
      stp x28, x29, [sp, #224]
      str x30, [sp, #240]

      mrs x0, spsr_el1
      mrs x1, elr_el1
      stp x0, x1, [sp, #248]

      bl handle_irq

      mov x0, sp
      bl schedule

      mov sp, x0

      ldp x0, x1, [sp, #248]
      msr spsr_el1, x0
      msr elr_el1, x1

      ldp x0, x1, [sp, #0]
      ldp x2, x3, [sp, #16]
      ldp x4, x5, [sp, #32]
      ldp x6, x7, [sp, #48]
      ldp x8, x9, [sp, #64]
      ldp x10, x11, [sp, #80]
      ldp x12, x13, [sp, #96]
      ldp x14, x15, [sp, #112]
      ldp x16, x17, [sp, #128]
      ldp x18, x19, [sp, #144]
      ldp x20, x21, [sp, #160]
      ldp x22, x23, [sp, #176]
      ldp x24, x25, [sp, #192]
      ldp x26, x27, [sp, #208]
      ldp x28, x29, [sp, #224]
      ldr x30, [sp, #240]

      add sp, sp, #272
      eret
      .balign 128
      
      b .
      .balign 128
      b .
      .balign 128

      // =========================================================
      // 3. Lower EL using AArch64 (4 vectors, 128 bytes each)
      // =========================================================
      
      // --- Slot 0: Synchronous Exception / System Call (svc) from User Space ---
      sub sp, sp, #272

      stp x0, x1, [sp, #0]
      stp x2, x3, [sp, #16]
      stp x4, x5, [sp, #32]
      stp x6, x7, [sp, #48]
      stp x8, x9, [sp, #64]
      stp x10, x11, [sp, #80]
      stp x12, x13, [sp, #96]
      stp x14, x15, [sp, #112]
      stp x16, x17, [sp, #128]
      stp x18, x19, [sp, #144]
      stp x20, x21, [sp, #160]
      stp x22, x23, [sp, #176]
      stp x24, x25, [sp, #192]
      stp x26, x27, [sp, #208]
      stp x28, x29, [sp, #224]
      str x30, [sp, #240]

      mrs x0, spsr_el1
      mrs x1, elr_el1
      stp x0, x1, [sp, #248]

      // Map syscall arguments for syscall_handler(sys_num, arg1, arg2, arg3)
      // x8 holds system call number, x0, x1, x2 hold arguments
      mov x3, x2
      mov x2, x1
      mov x1, x0
      mov x0, x8 
      bl syscall_handler

      // Save return value into saved x0 position on stack
      str x0, [sp, #0]

      ldp x0, x1, [sp, #248]
      msr spsr_el1, x0
      msr elr_el1, x1

      ldp x0, x1, [sp, #0]
      ldp x2, x3, [sp, #16]
      ldp x4, x5, [sp, #32]
      ldp x6, x7, [sp, #48]
      ldp x8, x9, [sp, #64]
      ldp x10, x11, [sp, #80]
      ldp x12, x13, [sp, #96]
      ldp x14, x15, [sp, #112]
      ldp x16, x17, [sp, #128]
      ldp x18, x19, [sp, #144]
      ldp x20, x21, [sp, #160]
      ldp x22, x23, [sp, #176]
      ldp x24, x25, [sp, #192]
      ldp x26, x27, [sp, #208]
      ldp x28, x29, [sp, #224]
      ldr x30, [sp, #240]

      add sp, sp, #272
      eret
      .balign 128

      // --- Slot 1: IRQ from Lower EL ---
      b .
      .balign 128
      // --- Slot 2: FIQ from Lower EL ---
      b .
      .balign 128
      // --- Slot 3: Error from Lower EL ---
      b .
      .balign 128

      // =========================================================
      // 4. Lower EL using AArch32 (4 vectors, 128 bytes each)
      // =========================================================
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

pub fn init() {
    unsafe {
        imp::init();

        #[cfg(target_arch = "aarch64")]
        {
            init_gic_timer_irq();
            init_aarch64_timer();
        }
    }
}

/// Interrupt and Exception Router for mitosOS.

/// Not currently wired to any IDT entry. `IDT.entries[0x80]` is set once,
/// above, to `syscall_handler_stub` -- the assembly stub that actually
/// saves/restores registers and matches `syscall_handler`'s calling
/// convention. This function used to *also* be installed at the same
/// vector (0x80 == 128), silently overwriting the working stub with a
/// handler that had no register-save prologue and would corrupt the trap
/// frame on return. Kept here, unwired, as a starting point if you switch
/// the syscall path to context-object-based dispatch later -- doing that
/// safely needs its own assembly stub (like `timer_handler_stub` builds a
/// `TaskContext`-shaped frame for `run_schedule`) rather than pointing the
/// IDT straight at a plain `extern "C" fn(&mut TaskContext)`.
#[allow(dead_code)]
#[cfg(target_arch = "x86_64")]
pub extern "C" fn x86_syscall_trap(frame: &mut crate::task::TaskContext) {
    // On x86_64, syscall arguments are typically passed via registers:
    // rax = syscall number, rdi = arg1, rsi = arg2, rdx = arg3
    let sys_num = frame.rax;
    let arg1 = frame.rdi;
    let arg2 = frame.rsi;
    let arg3 = frame.rdx;

    let ret = crate::syscall::syscall_handler(sys_num, arg1, arg2, arg3);
    frame.rax = ret; // Return value placed back in rax
}

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
