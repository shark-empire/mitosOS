//! SMP bring-up: starts every Application Processor (AP) MADT
//! reports, via the classic INIT-SIPI-SIPI sequence into a small
//! real-mode trampoline (src/smp_trampoline.s), landing each one in
//! Rust (rust_ap_entry, below) already in 64-bit long mode, with its
//! own per-CPU GDT/TSS (gdt::init_ap), the kernel's shared IDT
//! (interrupts::load_idt_this_core), and its own Local APIC enabled
//! (hal::apic).
//!
//! ## Why this must run before memory::unmap_low_half_identity_map()
//!
//! The trampoline starts in 16-bit real mode at a fixed low physical
//! page (TRAMPOLINE_PHYS) and, partway through, enables paging with a
//! `target_cr3` we hand it -- at that exact instant execution is
//! still physically at TRAMPOLINE_PHYS, so TRAMPOLINE_PHYS must still
//! be mapped (identity) in whatever page table `target_cr3` is, or
//! the very next instruction fetch after the `mov cr0, eax` that sets
//! CR0.PG faults with nothing set up yet to handle it.
//!
//! This module sidesteps building a dedicated AP page table entirely
//! by reusing the BSP's own *live* CR3 directly (see start_aps,
//! below) -- which, at the point main.rs calls this (right after
//! hal::init(), before memory::reclaim_boot_memory() and
//! memory::unmap_low_half_identity_map()), still has PML4[0] intact.
//! That low identity map is exactly what unmap_low_half_identity_map
//! tears down, later. The same live CR3 already maps the kernel's own
//! high-half code/data too, which is what makes the trampoline's
//! final jump into rust_ap_entry (a high-half virtual address) work
//! with no extra setup. Call this later than that main.rs ordering
//! and AP bring-up page-faults instantly on every core; call
//! memory::unmap_low_half_identity_map earlier than this and the same
//! thing happens for the same reason.
//!
//! ## Why this doesn't touch the 8259 PICs or IO-APIC interrupt routing
//!
//! interrupts.rs's remap_pic() already has the timer and UART IRQs
//! working, delivered to the BSP via the 8259's traditional
//! single-target INTR line ("virtual wire" mode, still valid until
//! something reprograms LINT0). Local APIC bring-up here doesn't
//! touch that path at all -- see hal::apic::init_this_core's doc
//! comment. Retiring the PICs in favor of IO-APIC-routed interrupts
//! (needed for any *other* core to receive a device interrupt, and
//! for per-CPU IPIs to be useful for anything beyond bring-up itself)
//! is real, separate follow-up work; hal::ioapic::set_redirection_entry
//! is already there and ready for it.

use crate::hal::madt::{self, MAX_CPUS};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

macro_rules! ulog {
    ($($arg:tt)*) => {{
        let mut uart = crate::uart::Uart::shared();
        let _ = core::fmt::Write::write_fmt(
            &mut uart,
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}

/// Fixed low physical page the trampoline is copied to and started
/// from. Chosen the same way every hobby/production trampoline picks
/// one: comfortably inside conventional memory, well below where an
/// EBDA ever realistically starts, and inside the range main.rs's PMM
/// setup already permanently reserves (frames 0..256, i.e. the whole
/// first 1MiB -- see memory::protect_boot_memory /
/// init_pmm_from_limine), so nothing else in the kernel will ever
/// hand this exact page out for any other purpose.
const TRAMPOLINE_PHYS: usize = 0x8000;

// Byte offsets of the trampoline's parameter block, *within*
// TRAMPOLINE_PHYS -- must match src/smp_trampoline.s's `params_*`
// labels' offsets exactly. That file pads out to this same 0x1F0
// before defining them specifically so both sides can hardcode the
// same numbers without needing a build-time symbol map.
const PARAM_CR3: usize = 0x1F0;
const PARAM_STACK_TOP: usize = 0x1F8;
const PARAM_ENTRY: usize = 0x200;
const PARAM_CPU_INDEX: usize = 0x208;

const TRAMPOLINE_BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smp_trampoline.bin"));

/// Per-AP kernel stack size. Deliberately smaller than task.rs's
/// per-task stacks -- these cores don't run a task yet, they just
/// idle until the scheduler grows CPU-aware -- so this stays small
/// rather than reserving more than is currently used.
const AP_STACK_SIZE: usize = 16 * 1024;

static AP_ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// Count of CPUs currently running kernel code, BSP included. Starts
/// at 1 (the BSP, already running by the time start_aps() is even
/// called) and gets one add per AP that actually comes up.
pub static CPUS_ONLINE: AtomicUsize = AtomicUsize::new(1);

fn write_param_u64(offset: usize, value: u64) {
    let addr = crate::memory::phys_to_virt(TRAMPOLINE_PHYS + offset) as *mut u64;
    unsafe { core::ptr::write_volatile(addr, value) };
}

unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Busy-waits ~`us` microseconds using PIT channel 2 in one-shot mode
/// -- the classic PC-speaker-timing trick (port 0x61's gate bit +
/// channel 2's terminal-count output). Entirely independent of
/// channel 0, which interrupts.rs's remap_pic()/init_pit() already
/// owns as the scheduler tick, so this never disturbs it and works
/// regardless of whether interrupts are currently enabled.
fn pit_delay_us(us: u32) {
    const PIT_HZ: u64 = 1_193_182;
    let count = (((PIT_HZ * us as u64) / 1_000_000).max(1)).min(0xFFFF) as u16;

    unsafe {
        let gate = (inb(0x61) & !0x02) | 0x01; // gate channel 2 on, speaker output off
        outb(0x61, gate);

        outb(0x43, 0xB0); // channel 2, lobyte/hibyte, mode 0, binary
        outb(0x42, (count & 0xFF) as u8);
        outb(0x42, (count >> 8) as u8);

        // Mode 0's OUT pin (port 0x61 bit 5) goes low the instant the
        // count above loads, and high again once it reaches zero --
        // that transition is the delay's end.
        while inb(0x61) & 0x20 == 0 {
            core::hint::spin_loop();
        }
    }
}

fn pit_delay_ms(ms: u32) {
    for _ in 0..ms {
        pit_delay_us(1000);
    }
}

/// Brings up every AP MADT reported. Safe to call even if MADT wasn't
/// found or reported no usable APs (logs and returns, leaving the
/// kernel single-core). See this module's doc comment for the exact
/// point in main.rs's boot sequence this must be called from.
pub fn start_aps() {
    let (local_apic_address, cpu_list, cpu_count) = {
        let data = madt::MADT.lock();
        if !data.found {
            ulog!("SMP: no MADT parsed, staying single-core");
            return;
        }
        (data.local_apic_address, data.cpus, data.cpu_count)
        // lock dropped here -- deliberately, before anything below
        // that takes more than a few cycles: Spinlock disables
        // interrupts for its whole held duration (see sync.rs), and
        // bring-up below spends multiple milliseconds per AP.
    };

    crate::hal::apic::set_base(local_apic_address);
    unsafe { crate::hal::apic::init_this_core() };
    crate::hal::ioapic::init_from_madt();

    let bsp_apic_id = crate::hal::apic::id();

    let target_cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) target_cr3, options(nomem, nostack));
    }

    // Copy the trampoline blob to its fixed physical page once; only
    // the parameter block within it (write_param_u64, below) changes
    // per AP.
    unsafe {
        let dest = crate::memory::phys_to_virt(TRAMPOLINE_PHYS) as *mut u8;
        core::ptr::copy_nonoverlapping(TRAMPOLINE_BLOB.as_ptr(), dest, TRAMPOLINE_BLOB.len());
    }

    let mut started = 0usize;
    let mut next_cpu_index = 0usize;

    for cpu in &cpu_list[..cpu_count] {
        if cpu.apic_id == bsp_apic_id {
            continue; // that's us -- already running, no SIPI needed
        }
        if !cpu.enabled {
            ulog!("SMP: skipping offline-but-hotpluggable CPU (APIC ID {})", cpu.apic_id);
            continue;
        }
        if next_cpu_index >= MAX_CPUS {
            ulog!("SMP: dropping CPU (APIC ID {}) -- MAX_CPUS ({}) reached", cpu.apic_id, MAX_CPUS);
            continue;
        }

        let cpu_index = next_cpu_index;
        next_cpu_index += 1;

        if start_one_ap(cpu.apic_id, cpu_index, target_cr3) {
            started += 1;
        }
    }

    CPUS_ONLINE.fetch_add(started, Ordering::SeqCst);
    ulog!("SMP: {} AP(s) started ({} CPU(s) online total)", started, 1 + started);
}

fn start_one_ap(apic_id: u32, cpu_index: usize, target_cr3: u64) -> bool {
    // Fresh per-AP kernel stack, leaked deliberately -- like
    // gdt.rs's DOUBLE_FAULT_STACK, a core's own stack is a permanent
    // resource for the OS's lifetime, never freed.
    let stack = alloc::vec![0u8; AP_STACK_SIZE].into_boxed_slice();
    let stack_top = alloc::boxed::Box::leak(stack).as_ptr() as u64 + AP_STACK_SIZE as u64;

    write_param_u64(PARAM_CR3, target_cr3 & !0xFFF);
    write_param_u64(PARAM_STACK_TOP, stack_top & !0xF); // 16-byte align, SysV ABI convention
    write_param_u64(PARAM_ENTRY, rust_ap_entry as usize as u64);
    write_param_u64(PARAM_CPU_INDEX, cpu_index as u64);

    AP_ONLINE[cpu_index].store(false, Ordering::SeqCst);

    let start_page = (TRAMPOLINE_PHYS >> 12) as u8;

    crate::hal::apic::send_init(apic_id);
    pit_delay_ms(10);

    crate::hal::apic::send_startup(apic_id, start_page);
    if !wait_online(cpu_index, 1) {
        // The original MP spec's double-SIPI exists for hardware old
        // enough that one Startup IPI isn't reliably enough to leave
        // "wait for SIPI" -- modern cores almost always are already
        // up by here, hence checking first rather than unconditionally
        // sending a second one.
        crate::hal::apic::send_startup(apic_id, start_page);
        wait_online(cpu_index, 100);
    }

    if AP_ONLINE[cpu_index].load(Ordering::SeqCst) {
        ulog!("SMP: AP APIC ID {} online as cpu {}", apic_id, cpu_index);
        true
    } else {
        ulog!("SMP: AP APIC ID {} did not respond, giving up", apic_id);
        false
    }
}

fn wait_online(cpu_index: usize, timeout_ms: u32) -> bool {
    for _ in 0..timeout_ms {
        if AP_ONLINE[cpu_index].load(Ordering::SeqCst) {
            return true;
        }
        pit_delay_ms(1);
    }
    false
}

/// Where every AP's trampoline hands off to Rust: 64-bit long mode,
/// its own stack (RSP already set by the trampoline), the BSP's page
/// tables (so all of the kernel's usual high-half code/data/HHDM is
/// already reachable), interrupts still off.
extern "C" fn rust_ap_entry(cpu_index: u64) -> ! {
    let cpu_index = cpu_index as usize;

    unsafe {
        crate::gdt::init_ap(cpu_index);
        crate::interrupts::load_idt_this_core();
        crate::hal::apic::init_this_core();
    }

    AP_ONLINE[cpu_index].store(true, Ordering::SeqCst);

    // No scheduler work is handed to APs yet (task.rs's run queue is
    // still BSP-only) -- park here, interrupts enabled so this core
    // can at least take IPIs later, until that's wired up.
    unsafe { crate::interrupts::enable_cpu_interrupts() };
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
