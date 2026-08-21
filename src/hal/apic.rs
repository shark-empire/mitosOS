//! Local APIC driver (xAPIC MMIO + x2APIC MSR interfaces).
//!
//! Every core has its own Local APIC (LAPIC) -- it's what actually
//! delivers interrupts (including IPIs, and the INIT/SIPI pair that
//! bring up an AP in the first place) to that specific core. This
//! module is deliberately narrow: enough to read the local ID, send
//! IPIs, and acknowledge interrupts (EOI), which is everything
//! hal::smp needs for AP bring-up. It does not touch LVT LINT0/LINT1
//! anywhere -- see init_this_core's doc comment for why.
//!
//! MMIO access goes through the same `phys_to_virt` (Limine's HHDM)
//! every other hal:: module already trusts for physical addresses
//! outside conventional RAM (hal::acpi does the same for the
//! EBDA/BIOS ROM range) -- Limine's HHDM covers the entire first 4GiB
//! of physical address space regardless of memory-map type, which is
//! exactly where both the Local APIC (0xFEE00000 by default) and the
//! IO-APIC live.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

macro_rules! ulog {
    ($($arg:tt)*) => {{
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_fmt(
            &mut uart,
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}

static LAPIC_PHYS_BASE: AtomicU64 = AtomicU64::new(0);
static USE_X2APIC: AtomicBool = AtomicBool::new(false);

/// xAPIC MMIO register byte offsets (Intel SDM Vol 3, Table 11-1).
mod reg {
    pub const ID: usize = 0x20;
    pub const EOI: usize = 0xB0;
    pub const SPURIOUS: usize = 0xF0;
    pub const ICR_LOW: usize = 0x300;
    pub const ICR_HIGH: usize = 0x310;
}

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const IA32_X2APIC_ID: u32 = 0x802;
const IA32_X2APIC_EOI: u32 = 0x80B;
const IA32_X2APIC_SIVR: u32 = 0x80F;
const IA32_X2APIC_ICR: u32 = 0x830;

/// Vector this module enables the Local APIC with -- 0xFF, the
/// conventional choice (Intel SDM recommends the low nibble be all
/// 1s on P6+ hardware, which 0xFF trivially satisfies) and clear of
/// every vector interrupts.rs currently installs a handler at
/// (0x00-0x0E, 0x20, 0x24, 0x80). No IDT gate is installed at 0xFF
/// itself yet, so a genuinely spurious interrupt (rare, and mostly a
/// theoretical concern in QEMU) would currently fall through to the
/// #GP handler instead of being silently absorbed the way the
/// spurious-vector mechanism intends -- a real gate here (one that
/// just `iretq`s, no EOI needed) is a small, self-contained follow-up.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((hi as u64) << 32) | lo as u64
}

unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") (value & 0xFFFF_FFFF) as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// CPUID.01H:ECX -- we only ever need bit 21 (x2APIC support). ebx is
/// saved/restored around the instruction rather than declared as a
/// clobber: under some relocation models LLVM reserves rbx for its
/// own use and won't allow inline asm to touch it directly, and
/// saving it explicitly is correct regardless of relocation model.
unsafe fn cpuid_ecx1() -> u32 {
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => _,
            out("ecx") ecx,
            out("edx") _,
            // Not nomem/nostack: the push/pop above genuinely touch
            // the stack, unlike every other asm! block in this file.
            options(preserves_flags)
        );
    }
    ecx
}

fn mmio_read(offset: usize) -> u32 {
    let base = LAPIC_PHYS_BASE.load(Ordering::SeqCst) as usize;
    let addr = crate::memory::phys_to_virt(base + offset) as *const u32;
    unsafe { core::ptr::read_volatile(addr) }
}

fn mmio_write(offset: usize, value: u32) {
    let base = LAPIC_PHYS_BASE.load(Ordering::SeqCst) as usize;
    let addr = crate::memory::phys_to_virt(base + offset) as *mut u32;
    unsafe { core::ptr::write_volatile(addr, value) }
}

/// Reads this core's own Local APIC ID -- the value SIPI/IPI target
/// addressing and MADT's Processor Local APIC entries both refer to.
pub fn id() -> u32 {
    if USE_X2APIC.load(Ordering::SeqCst) {
        (unsafe { rdmsr(IA32_X2APIC_ID) }) as u32
    } else {
        mmio_read(reg::ID) >> 24
    }
}

/// One-time setup shared by every core: decides xAPIC vs x2APIC (once,
/// from the BSP -- every core on a real system agrees, this isn't
/// re-probed per AP) and records the Local APIC's MMIO base from
/// MADT (or its type-5 override). Call once from the BSP, before
/// calling init_this_core() anywhere.
pub fn set_base(phys_base: u64) {
    LAPIC_PHYS_BASE.store(phys_base, Ordering::SeqCst);
    let x2apic_supported = unsafe { cpuid_ecx1() } & (1 << 21) != 0;
    USE_X2APIC.store(x2apic_supported, Ordering::SeqCst);
    if x2apic_supported {
        ulog!("APIC: x2APIC supported, using MSR interface");
    } else {
        ulog!("APIC: x2APIC not supported, using xAPIC MMIO interface");
    }
}

/// Enables this core's Local APIC (the software-enable bit in the
/// spurious-interrupt vector register) and, if the system supports
/// it, switches IA32_APIC_BASE into x2APIC mode first. Idempotent --
/// safe to call more than once on the same core.
///
/// Deliberately does **not** touch LVT LINT0/LINT1 or TPR: on the
/// BSP, firmware has already wired LINT0 for the legacy 8259 PIC's
/// "virtual wire" delivery, which is what interrupts.rs's
/// remap_pic() and the working PIT/UART IRQs currently depend on --
/// this function must stay a no-op against that. On a freshly-started
/// AP the LVT entries reset masked, which is exactly what's wanted:
/// these cores don't own any legacy IRQ line (yet -- see hal::smp's
/// module doc comment for the follow-up that would change this).
pub unsafe fn init_this_core() {
    unsafe {
        let base = rdmsr(IA32_APIC_BASE_MSR);
        let mut new_base = base | (1 << 11); // xAPIC global enable
        if USE_X2APIC.load(Ordering::SeqCst) {
            new_base |= 1 << 10; // x2APIC enable (requires bit 11 also set)
        }
        if new_base != base {
            wrmsr(IA32_APIC_BASE_MSR, new_base);
        }

        if USE_X2APIC.load(Ordering::SeqCst) {
            wrmsr(IA32_X2APIC_SIVR, (1u64 << 8) | SPURIOUS_VECTOR as u64);
        } else {
            mmio_write(reg::SPURIOUS, (1u32 << 8) | SPURIOUS_VECTOR as u32);
        }
    }
}

/// Signals end-of-interrupt for whatever this core is currently
/// servicing.
#[allow(dead_code)]
pub fn eoi() {
    if USE_X2APIC.load(Ordering::SeqCst) {
        unsafe { wrmsr(IA32_X2APIC_EOI, 0) };
    } else {
        mmio_write(reg::EOI, 0);
    }
}

/// Waits for a previously-issued xAPIC ICR command to finish sending
/// (bit 12, delivery status). x2APIC's ICR is a single atomic MSR
/// write with no equivalent pending state, so this is a no-op there.
fn wait_icr_idle() {
    if USE_X2APIC.load(Ordering::SeqCst) {
        return;
    }
    while mmio_read(reg::ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
}

fn send_icr(dest_apic_id: u32, icr_low_bits: u32) {
    unsafe {
        if USE_X2APIC.load(Ordering::SeqCst) {
            // x2APIC's ICR is one 64-bit MSR: destination in the high
            // 32 bits (the full 32-bit APIC ID, no shifting into
            // bits 63:56 the way xAPIC's separate ICR_HIGH needs),
            // the same low-32 command encoding as xAPIC below.
            let value = ((dest_apic_id as u64) << 32) | icr_low_bits as u64;
            wrmsr(IA32_X2APIC_ICR, value);
        } else {
            wait_icr_idle();
            mmio_write(reg::ICR_HIGH, (dest_apic_id & 0xFF) << 24);
            mmio_write(reg::ICR_LOW, icr_low_bits);
            wait_icr_idle();
        }
    }
}

/// Sends an INIT IPI to `dest_apic_id` -- the first step of AP
/// bring-up, resetting the target core to a well-defined state and
/// putting it in "wait for SIPI".
pub fn send_init(dest_apic_id: u32) {
    // ICR bits: [10:8]=delivery mode (101=INIT), [14]=level (1=assert),
    // [15]=trigger mode (0=edge). This is the simplified, modern
    // sequence (SDM Vol 3, 8.4.4.1) -- no separate level-deassert
    // step, which only matters for pre-P6 hardware.
    const INIT_ASSERT: u32 = (0b101 << 8) | (1 << 14);
    send_icr(dest_apic_id, INIT_ASSERT);
}

/// Sends a Startup IPI (SIPI) pointing `dest_apic_id` at `start_page`,
/// the trampoline's physical page number (`TRAMPOLINE_PHYS >> 12`) --
/// the AP begins execution at `start_page << 12` in real mode. Must
/// only be sent after send_init and its ~10ms settle delay (see
/// hal::smp).
pub fn send_startup(dest_apic_id: u32, start_page: u8) {
    const STARTUP_ASSERT: u32 = (0b110 << 8) | (1 << 14);
    send_icr(dest_apic_id, STARTUP_ASSERT | start_page as u32);
}
