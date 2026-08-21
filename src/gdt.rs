//! src/gdt.rs -- x86_64-only. Declare with `#[cfg(target_arch = "x86_64")]
//! pub mod gdt;` in main.rs (same pattern as `pci`), not a bare `mod gdt;`.
//!
//! Whatever bootloader got the kernel here builds only a minimal GDT of
//! its own, just enough to reach long mode -- null / ring-0 code /
//! ring-0 data, nothing else -- and this module replaces it outright
//! (see `init` below: it builds and loads its own GDT unconditionally,
//! never reads whatever was active first) with a permanent one that
//! adds ring-3 segments and a TSS. Until this runs, there's no ring-3
//! or TSS, which is the real reason every task so far runs at full
//! kernel privilege (see task.rs::Task::init hardcoding cs=0x08,
//! ss=0x10 for every task, "isolated" or not) -- 0x08/0x10 being this
//! module's own choice of selector layout, not the bootloader's.
//!
//! This module gives the kernel its own permanent GDT with:
//!   - the SAME selectors 0x08 / 0x10 for ring-0 code/data, so nothing
//!     else (IDT gates all hardcode gdt_selector=0x08, task.rs's kernel
//!     context) needs to change
//!   - new ring-3 code/data selectors, for actual user-mode execution
//!   - a TSS, so the CPU knows which kernel stack to load (RSP0) on any
//!     interrupt/exception/syscall taken from ring 3, plus a dedicated
//!     IST1 stack for double faults specifically
//!
//! Scope note: this builds the mechanism and proves it end-to-end via
//! `enter_usermode`, but doesn't yet update RSP0 on every context switch
//! (that needs a per-task kernel stack in task.rs) and nothing calls
//! `enter_usermode` yet -- that's the next step, wiring this into
//! spawn_from_elf. `set_kernel_stack` is exposed now so that step doesn't
//! need to come back and touch this file again.

use core::mem::size_of;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_CODE_SELECTOR: u16 = 0x18 | 3; // RPL=3
pub const USER_DATA_SELECTOR: u16 = 0x20 | 3; // RPL=3
const TSS_SELECTOR: u16 = 0x28;

/// Index into Tss::ist (0-based) used for double faults. The IDT gate's
/// IST field (see interrupts.rs's new `set_ist`) uses 1-based numbering
/// (IST1..IST7), so ist[DOUBLE_FAULT_IST_INDEX] == "IST1".
const DOUBLE_FAULT_IST_INDEX: usize = 0;
pub const DOUBLE_FAULT_IST_NUMBER: u8 = 1;

const DF_STACK_SIZE: usize = 8192;

#[repr(align(16))]
#[allow(dead_code)] // only ever written (zero-init) and address-taken -- the
                     // CPU reads this memory directly as the IST1 stack
                     // during a double fault, never through Rust code
struct IstStack([u8; DF_STACK_SIZE]);
static mut DOUBLE_FAULT_STACK: IstStack = IstStack([0; DF_STACK_SIZE]);

// ---------------------------------------------------------------------
// TSS
// ---------------------------------------------------------------------

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

impl Tss {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            // Points past the end of the TSS -- "no I/O permission bitmap",
            // so ring-3 code can never do raw port I/O, only via syscalls.
            iomap_base: size_of::<Tss>() as u16,
        }
    }
}

static mut TSS: Tss = Tss::new();

// ---------------------------------------------------------------------
// GDT
// ---------------------------------------------------------------------

#[derive(Clone, Copy)]
#[repr(transparent)]
struct GdtEntry(u64);

impl GdtEntry {
    const fn null() -> Self {
        GdtEntry(0)
    }

    /// Flat (base=0, limit=4GB) code/data segment. In long mode the CPU
    /// ignores base/limit for these anyway (paging does all the real
    /// address translation) -- what actually matters is `access`
    /// (present/DPL/type) and, for code segments, the L bit in `flags`.
    const fn flat(access: u8, flags: u8) -> Self {
        GdtEntry(descriptor_bits(0, 0xFFFFF, access, flags))
    }
}

const fn descriptor_bits(base: u32, limit: u32, access: u8, flags: u8) -> u64 {
    let limit_low = (limit & 0xFFFF) as u64;
    let limit_high = ((limit >> 16) & 0xF) as u64;
    let base_low = (base & 0xFFFF) as u64;
    let base_mid = ((base >> 16) & 0xFF) as u64;
    let base_high = ((base >> 24) & 0xFF) as u64;
    limit_low
        | (base_low << 16)
        | (base_mid << 32)
        | ((access as u64) << 40)
        | (limit_high << 48)
        | ((flags as u64) << 52)
        | (base_high << 56)
}

/// The TSS descriptor is a 16-byte *system* descriptor (needs a full
/// 64-bit base), so it takes two consecutive GDT slots.
const fn tss_descriptor_bits(base: u64, limit: u32) -> (u64, u64) {
    // access=0x89: Present, DPL0, Type=0b1001 (64-bit TSS, available).
    // flags=0x0: byte granularity, reserved bits clear.
    let low = descriptor_bits((base & 0xFFFF_FFFF) as u32, limit, 0x89, 0x0);
    let high = (base >> 32) & 0xFFFF_FFFF;
    (low, high)
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Gdt {
    null: GdtEntry,
    kernel_code: GdtEntry,
    kernel_data: GdtEntry,
    user_code: GdtEntry,
    user_data: GdtEntry,
    tss_low: GdtEntry,
    tss_high: GdtEntry,
}

static mut GDT: Gdt = Gdt {
    null: GdtEntry::null(),
    kernel_code: GdtEntry::flat(0x9A, 0xA), // P, DPL0, code, exec+readable, long-mode (L=1)
    kernel_data: GdtEntry::flat(0x92, 0xC), // P, DPL0, data, writable
    user_code: GdtEntry::flat(0xFA, 0xA),   // P, DPL3, code, exec+readable, long-mode (L=1)
    user_data: GdtEntry::flat(0xF2, 0xC),   // P, DPL3, data, writable
    // Patched in init() -- the TSS's address isn't knowable at const-eval
    // time, only once it's actually laid out in memory.
    tss_low: GdtEntry::null(),
    tss_high: GdtEntry::null(),
};

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

unsafe extern "C" {
    fn gdt_flush(ptr: *const DescriptorTablePointer);
}

// lgdt doesn't reload CS by itself -- x86_64 has no `mov cs, reg`, so the
// standard trick is a far return: push the new CS + a return address,
// then `retfq` pops both and reloads CS atomically. SS/DS/ES/FS/GS *can*
// be reloaded with plain `mov`, once we're back at the same privilege
// level we started at (ring 0 -> ring 0 here, so this part's simple; the
// ring-3 case in enter_usermode below is the one where SS has to go
// through iretq instead, see the comment there).
core::arch::global_asm!(
    r#"
    .section .text
    .global gdt_flush
    gdt_flush:
        lgdt [rdi]
        push 0x08
        lea rax, [rip + 2f]
        push rax
        retfq
    2:
        mov ax, 0x10
        mov ds, ax
        mov es, ax
        mov fs, ax
        mov gs, ax
        mov ss, ax
        ret
    "#
);

/// Builds the TSS descriptor, loads the new GDT (reloading every segment
/// register to point at it), and loads the TSS with `ltr`. Call once,
/// early in boot -- **before** `interrupts::init()` enables interrupts,
/// since the double-fault gate is set to use IST1 (set up here) and any
/// #DF between `interrupts::init()` and this running would have nowhere
/// valid to switch its stack to.
pub unsafe fn init() {
    unsafe {
        let df_stack_top = (&raw const DOUBLE_FAULT_STACK as u64) + DF_STACK_SIZE as u64;
        (&raw mut TSS.ist[DOUBLE_FAULT_IST_INDEX]).write(df_stack_top);

        let tss_base = &raw const TSS as u64;
        let tss_limit = (size_of::<Tss>() - 1) as u32;
        let (low, high) = tss_descriptor_bits(tss_base, tss_limit);
        (&raw mut GDT.tss_low).write(GdtEntry(low));
        (&raw mut GDT.tss_high).write(GdtEntry(high));

        let ptr = DescriptorTablePointer {
            limit: (size_of::<Gdt>() - 1) as u16,
            base: &raw const GDT as u64,
        };
        gdt_flush(&ptr);

        core::arch::asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, nomem));
    }
}

/// Points RSP0 -- the stack the CPU switches to on any trap taken from
/// ring 3 -- at `stack_top`. Call this on every context switch once tasks
/// have their own per-task kernel stacks (task.rs, next step); until
/// then, call it once at boot with any valid kernel stack so the
/// mechanism is at least non-null before anything hits ring 3.
pub fn set_kernel_stack(stack_top: u64) {
    unsafe {
        (&raw mut TSS.rsp[0]).write(stack_top);
    }
}

/// Transitions into ring 3 at `entry`, with `user_stack_top` as the
/// initial RSP. Never returns to the caller -- the only way back into
/// this Rust code is through a trap (syscall, fault, timer tick), which
/// re-enters via whatever IDT gate that vector points at, using RSP0
/// from the TSS as its stack.
///
/// `entry` and `user_stack_top` must both already be mapped **and marked
/// user-accessible** in the currently-loaded page table (see
/// `vmm::MapFlags`) -- this function only performs the privilege
/// transition itself, it doesn't map anything.
///
/// Not currently called: `task::spawn_from_elf` reaches ring 3 through
/// `Task::init`'s context-frame + scheduler `iretq` path instead (see
/// its doc comment), so a process's first entry into ring 3 goes
/// through the same generic restore path as every later context
/// switch, rather than a separate one-shot jump. Kept, `unsafe fn` and
/// unwired, as this module's own doc comment already flags -- a direct
/// jump like this is what a non-scheduler-mediated ring-3 entry would
/// use, the same role `process.rs`'s old `enter_user_mode` used to
/// fill before that was replaced by `task::spawn_from_elf` (see
/// `process.rs`'s module doc comment).
#[allow(dead_code)]
pub unsafe fn enter_usermode(entry: usize, user_stack_top: usize) -> ! {
    unsafe {
        core::arch::asm!(
            // DS/ES/FS/GS can be set directly here, at CPL=0: the rule
            // for loading a data-segment register is max(CPL, RPL) <=
            // DPL, and 0 and 3 both satisfy that against a DPL=3
            // descriptor. SS is the one exception to that rule -- SS's
            // DPL must *equal* CPL exactly, so it can't be set with a
            // plain `mov` here (CPL is still 0). It's set atomically by
            // iretq below instead, at the point CPL actually becomes 3.
            "mov ax, {data_sel:x}",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            // iretq pops, in order, RIP / CS / RFLAGS / RSP / SS -- so
            // they have to be pushed in the reverse order (SS first,
            // RIP last) for RIP to end up on top of the stack.
            "push {data_sel}",  // SS
            "push {stack}",     // RSP
            "push 0x202",       // RFLAGS: IF=1, bit1 reserved-set
            "push {code_sel}",  // CS
            "push {entry}",     // RIP
            "iretq",
            data_sel = in(reg) USER_DATA_SELECTOR as u64,
            code_sel = in(reg) USER_CODE_SELECTOR as u64,
            stack = in(reg) user_stack_top as u64,
            entry = in(reg) entry as u64,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------
// Per-CPU GDT/TSS (APs)
// ---------------------------------------------------------------------
//
// Every core needs its own GDT purely because of the TSS: a TSS
// descriptor's "busy" bit is set by `ltr` and would collide if two
// cores ever pointed their TR at the very same descriptor
// simultaneously -- each core needs its own TSS *descriptor* (even if
// the code/data descriptor *values* around it are identical). The
// code/data descriptor values below are deliberately identical to
// BSP's own GDT (init(), above): interrupts.rs's IDT gates hardcode
// gdt_selector=0x08 for every vector, on every core, so 0x08/0x10/...
// have to mean the same thing (flat ring-0 code/data) in every
// per-CPU GDT for that to keep working.

use crate::hal::madt::MAX_CPUS;

const AP_DF_STACK_SIZE: usize = 8192;

#[derive(Clone, Copy)]
struct PerCpu {
    gdt: Gdt,
    tss: Tss,
}

impl PerCpu {
    const fn new() -> Self {
        Self {
            gdt: Gdt {
                null: GdtEntry::null(),
                kernel_code: GdtEntry::flat(0x9A, 0xA),
                kernel_data: GdtEntry::flat(0x92, 0xC),
                user_code: GdtEntry::flat(0xFA, 0xA),
                user_data: GdtEntry::flat(0xF2, 0xC),
                // Patched in init_ap(), same reason as BSP's GDT above.
                tss_low: GdtEntry::null(),
                tss_high: GdtEntry::null(),
            },
            tss: Tss::new(),
        }
    }
}

static mut AP_GDTS: [PerCpu; MAX_CPUS] = [PerCpu::new(); MAX_CPUS];

/// Per-CPU double-fault (IST1) stacks -- one per possible AP, so a #DF
/// on one core can never clobber another's in-flight #DF, the same
/// reasoning DOUBLE_FAULT_STACK above exists for the BSP. Indexed by
/// the same `cpu_index` hal::smp assigns each AP.
#[derive(Clone, Copy)]
#[repr(align(16))]
struct ApDfStack([u8; AP_DF_STACK_SIZE]);

static mut AP_DF_STACKS: [ApDfStack; MAX_CPUS] = [ApDfStack([0; AP_DF_STACK_SIZE]); MAX_CPUS];

/// Builds, loads, and activates `cpu_index`'s own GDT + TSS. Called
/// once by each AP, from hal::smp's rust_ap_entry, before that core
/// does anything that could fault -- the shared IDT's #DF gate
/// (interrupts.rs) points at IST1 on whichever TSS is currently
/// loaded, so a valid per-CPU TSS has to be in place before this core
/// loads that IDT (interrupts::load_idt_this_core) or unmasks
/// interrupts.
///
/// # Safety
/// `cpu_index` must be `< hal::madt::MAX_CPUS` and unique per core --
/// hal::smp assigns these and guarantees both. Reusing an index across
/// two cores would let them share a TSS, which is unsound (see this
/// section's doc comment).
pub unsafe fn init_ap(cpu_index: usize) {
    unsafe {
        let gdt_ptr: *mut PerCpu = (&raw mut AP_GDTS).cast::<PerCpu>().add(cpu_index);
        let stack_ptr: *mut ApDfStack = (&raw mut AP_DF_STACKS).cast::<ApDfStack>().add(cpu_index);

        let df_stack_top = stack_ptr as u64 + AP_DF_STACK_SIZE as u64;
        (&raw mut (*gdt_ptr).tss.ist[DOUBLE_FAULT_IST_INDEX]).write(df_stack_top);

        let tss_base = (&raw const (*gdt_ptr).tss) as u64;
        let tss_limit = (size_of::<Tss>() - 1) as u32;
        let (low, high) = tss_descriptor_bits(tss_base, tss_limit);
        (&raw mut (*gdt_ptr).gdt.tss_low).write(GdtEntry(low));
        (&raw mut (*gdt_ptr).gdt.tss_high).write(GdtEntry(high));

        let ptr = DescriptorTablePointer {
            limit: (size_of::<Gdt>() - 1) as u16,
            base: (&raw const (*gdt_ptr).gdt) as u64,
        };
        gdt_flush(&ptr);

        core::arch::asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, nomem));
    }
}
