//! Professional Production-Ready Memory Subsystem for mitosOS.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_arch = "x86_64")]
use core::sync::atomic::AtomicUsize;

// =========================================================================
// 0. PHYSICAL <-> VIRTUAL TRANSLATION
// =========================================================================
//
// This kernel is higher-half only on x86_64, and needs a way to turn a
// bare physical address (straight off CR3, a fresh `vmm_alloc_frame()`,
// a `PageTableEntry::physical_address()`, ...) into a dereferenceable
// pointer -- there is no permanent identity map. Unlike a fixed
// compile-time offset, this can't be a constant: which offset is
// correct depends on which bootloader is in play, and for Limine it's
// a bootloader-chosen value only known at runtime.
//
// - Limine boots (src/limine.rs): Limine provides its own Higher Half
//   Direct Map (HHDM) at an offset of ITS choosing -- the protocol is
//   explicit that this "may vary between boots" and must be queried,
//   never assumed. main.rs queries it (limine::hhdm_offset()) and
//   calls `set_hhdm_offset` with the real value before anything else
//   runs that might dereference a physical address.
// - Multiboot2 boots (src/boot_multiboot2.s): the trampoline's own
//   identity-mapping page tables additionally mirror physical [0,
//   1GiB) at PML4 index 256 (the same slot, and same 0xFFFF800000000000
//   virtual base, this offset defaults to below) -- specifically so
//   that default is already correct and nothing needs to call
//   `set_hhdm_offset` on that path at all. See that file's comments
//   for why this is a second, persistent alias rather than reusing
//   the (deliberately temporary -- see `unmap_low_half_identity_map`)
//   identity mapping at PML4[0] directly.
//
// AArch64 doesn't need any of this: mmu.rs keeps a permanent flat
// identity map (kernel RAM + MMIO, all under L0 index 0) that is never
// torn down, so a bare physical address is already a valid pointer
// there. `phys_to_virt` is a no-op on that target -- every call site
// below can use it unconditionally without `#[cfg]`.

/// Current physical->virtual offset for x86_64. Defaults to the
/// address Multiboot2 boots need (see the module comment); Limine
/// boots override this via `set_hhdm_offset` with the bootloader's
/// actual (and boot-to-boot variable) HHDM offset before anything
/// else runs that might call `phys_to_virt`.
#[cfg(target_arch = "x86_64")]
pub static HHDM_OFFSET: AtomicUsize = AtomicUsize::new(0xFFFF_8000_0000_0000);

/// Sets the offset `phys_to_virt` uses from this point on. Must be
/// called, if at all, before anything might call `phys_to_virt` --
/// which in practice means as close to the top of kmain as possible.
/// See the module comment: only a Limine boot needs to call this.
#[cfg(target_arch = "x86_64")]
pub fn set_hhdm_offset(offset: usize) {
    HHDM_OFFSET.store(offset, Ordering::SeqCst);
}

/// Current physical->virtual offset (see `set_hhdm_offset`). Exists so
/// other modules that need the raw offset itself (pci.rs's DMA/HAL
/// setup, currently) don't reach into the atomic directly.
#[cfg(target_arch = "x86_64")]
pub fn hhdm_offset() -> usize {
    HHDM_OFFSET.load(Ordering::SeqCst)
}

/// Turns a physical address into a pointer the CPU can dereference
/// right now. See the module-level comment above for why this is
/// needed on x86_64 and a no-op on aarch64.
#[inline(always)]
pub fn phys_to_virt(phys: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        phys + HHDM_OFFSET.load(Ordering::SeqCst)
    }
    #[cfg(target_arch = "aarch64")]
    {
        phys
    }
}

/// Hardware-agnostic memory mapping flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapFlags {
    pub writable: bool,
    pub user_accessible: bool,
    pub execute_disable: bool,
    /// AArch64 only: selects the Device-nGnRnE MAIR_EL1 attribute index
    /// instead of Normal Write-Back (see mmu.rs). Ignored on x86_64,
    /// which has no memory-type distinction wired up yet. Needed
    /// because ARM's Device memory rules (no unaligned/speculative
    /// access) are a strict superset of Normal's restrictions -- MMIO
    /// mapped as Normal can silently misbehave, and marking ordinary
    /// RAM as Device would be safe but needlessly slow.
    pub device: bool,
}

impl MapFlags {
    pub const fn kernel_code() -> Self {
        Self { writable: false, user_accessible: false, execute_disable: false, device: false }
    }

    pub const fn kernel_data() -> Self {
        Self { writable: true, user_accessible: false, execute_disable: true, device: false }
    }
}

// =========================================================================
// 1. CONSTANTS & SECURITY
// =========================================================================

const PAGE_SIZE: usize = 4096;
const BUCKET_COUNT: usize = 9;
const MIN_BLOCK_SIZE: usize = core::mem::size_of::<*mut ListNode>();

static INITIALIZED: AtomicBool = AtomicBool::new(false);

// =========================================================================
// 2. SYNCHRONIZATION
// =========================================================================

pub struct Mutex<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Mutex {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    #[inline(always)]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        while self.lock.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        MutexGuard { mutex: self }
    }
}

pub struct MutexGuard<'a, T> { mutex: &'a Mutex<T> }
impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.mutex.data.get() } }
}
impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T { unsafe { &mut *self.mutex.data.get() } }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) { self.mutex.lock.store(false, Ordering::Release); }
}

// =========================================================================
// 3. HEAP ALLOCATOR
// =========================================================================

struct ListNode { next: *mut ListNode }

pub struct FastBlockAllocator {
    buckets: [*mut ListNode; BUCKET_COUNT],
    heap_start: usize,
    heap_end: usize,
    next_free_byte: usize,
}

unsafe impl Send for FastBlockAllocator {}

impl FastBlockAllocator {
    pub const fn new() -> Self {
        FastBlockAllocator {
            buckets: [ptr::null_mut(); BUCKET_COUNT],
            heap_start: 0,
            heap_end: 0,
            next_free_byte: 0,
        }
    }

    pub unsafe fn init(&mut self, start: usize, size: usize) {
        self.heap_start = start;
        self.heap_end = start + size;
        self.next_free_byte = start;
        INITIALIZED.store(true, Ordering::SeqCst);
    }

    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        let size = layout.size();
        let alloc_start = (self.next_free_byte + align - 1) & !(align - 1);
        let alloc_end = match alloc_start.checked_add(size) {
            Some(end) if end <= self.heap_end => end,
            _ => return ptr::null_mut(),
        };
        self.next_free_byte = alloc_end;
        // `heap_start`/`heap_end`/`next_free_byte` are tracked as plain
        // physical-style offsets (see HEAP_START below); translate only
        // the final pointer we actually hand back to the caller. Freed
        // blocks recycled through the bucket free-lists above never hit
        // this path again, so they don't need a second translation --
        // they're already whatever this function returned the first time.
        phys_to_virt(alloc_start) as *mut u8
    }
}

#[inline]
fn target_bucket_index(layout: &Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).next_power_of_two();
    if size > 2048 { None } else { Some((size.trailing_zeros() as usize).saturating_sub(3)) }
}

#[global_allocator]
static HEAP_ALLOCATOR: Mutex<FastBlockAllocator> = Mutex::new(FastBlockAllocator::new());

unsafe impl GlobalAlloc for Mutex<FastBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !INITIALIZED.load(Ordering::SeqCst) { return ptr::null_mut(); }

        let size = layout.size().max(MIN_BLOCK_SIZE);
        let layout = Layout::from_size_align(size, layout.align()).unwrap();
        let mut allocator = self.lock();

        let ptr = if let Some(index) = target_bucket_index(&layout) {
            if !allocator.buckets[index].is_null() {
                let node = allocator.buckets[index];
                unsafe { allocator.buckets[index] = (*node).next; }
                node as *mut u8
            } else {
                allocator.fallback_alloc(layout)
            }
        } else {
            allocator.fallback_alloc(layout)
        };

        if !ptr.is_null() {
            unsafe { ptr::write_bytes(ptr, 0, size); }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() { return; }
        let size = layout.size().max(MIN_BLOCK_SIZE);
        let layout = Layout::from_size_align(size, layout.align()).unwrap();
        
        let mut allocator = self.lock();
        if let Some(index) = target_bucket_index(&layout) {
            let node = ptr as *mut ListNode;
            unsafe {
                (*node).next = allocator.buckets[index];
                allocator.buckets[index] = node;
            }
        }
    }
}

// =========================================================================
// 4. PHYSICAL MEMORY MANAGER & INITIALIZATION
// =========================================================================

pub struct BitmapAllocator<const N: usize> { bitmap: [u64; N] }
impl<const N: usize> BitmapAllocator<N> {
    pub const fn new() -> Self { Self { bitmap: [0; N] } }
    
    pub fn allocate_next_frame(&mut self) -> Option<usize> {
        for (i, val) in self.bitmap.iter_mut().enumerate() {
            if *val != !0 {
                let bit = (!*val).trailing_zeros() as usize;
                *val |= 1 << bit;
                return Some(i * 64 + bit);
            }
        }
        None
    }

    pub fn reserve_frame(&mut self, frame_index: usize) {
        let array_idx = frame_index / 64;
        let bit_idx = frame_index % 64;
        if array_idx < N { self.bitmap[array_idx] |= 1 << bit_idx; }
    }

    /// Returns a frame to the pool. Mirrors `reserve_frame`'s bounds
    /// check; out-of-range indices are silently ignored rather than
    /// panicking, since a bad address here would otherwise take the
    /// whole kernel down over a bookkeeping bug in a caller.
    pub fn free_frame(&mut self, frame_index: usize) {
        let array_idx = frame_index / 64;
        let bit_idx = frame_index % 64;
        if array_idx < N { self.bitmap[array_idx] &= !(1 << bit_idx); }
    }

    pub fn reserve_range(&mut self, start_frame: usize, count: usize) {
        for i in 0..count { self.reserve_frame(start_frame + i); }
    }
}

pub static PHYSICAL_PMM: Mutex<BitmapAllocator<1024>> = Mutex::new(BitmapAllocator::new());

/// Bridge for the VMM
/// Start of the region every process's private (ELF + stack) mappings
/// must live in -- currently enforced by elf.rs rejecting any PT_LOAD
/// segment below this, and by convention in task::allocate_user_stack.
///
/// This is `1 << 39`: the first byte of PML4 entry 1 on x86_64 / L0
/// entry 1 on AArch64 (each top-level entry spans 512GB). The kernel's
/// own identity map -- a few MiB on x86_64, 32MiB on AArch64 -- lives
/// entirely inside entry 0, alongside nothing else, by design.
///
/// The reason this constant exists at all: create_process_page_table
/// gives every process a *copy* of the kernel's own top-level table,
/// so entry 0 (and only entry 0, since nothing else is populated yet)
/// starts out shared -- same physical L1/L2/L3 sub-tables as the
/// kernel and as every other process, which is exactly what's wanted
/// for the kernel's own mappings to stay reachable after a TTBR/CR3
/// switch. But "shared" cuts both ways: if a process's own private
/// mappings (ELF segments, user stack) *also* landed under entry 0 --
/// which they used to, back when the user stack sat at a fixed low
/// address and ELF binaries linked near 0x400000, both comfortably
/// inside the first 512GB -- walking down to add those mappings would
/// walk into the *same shared* sub-tables, silently splicing one
/// process's "private" pages into the structure every other process
/// and the kernel itself also uses. Two processes with overlapping
/// virtual addresses (entirely plausible -- most things link low by
/// default) would alias each other's memory instead of being isolated
/// from it, and a process's PT_LOAD could just as easily corrupt the
/// kernel's own identity-mapped entries.
///
/// Every top-level entry from 1 onward is unpopulated in the kernel's
/// own table (nothing kernel-side has ever needed an address that
/// high), so it's unpopulated in every process's copy too -- meaning
/// the *first* mapping any process makes above this line forces a
/// brand-new, private L1/L2/L3 chain for that one process, with
/// nothing shared. That's the actual isolation boundary; the
/// top-level-table copy on its own only ever provided sharing.
pub const USER_SPACE_BASE: usize = 1 << 39;

pub fn vmm_alloc_frame() -> Option<usize> {
    PHYSICAL_PMM.lock().allocate_next_frame().map(|idx| idx * PAGE_SIZE)
}

/// Returns a physical frame to the allocator. `addr` must be
/// page-aligned and must not still be referenced by any live page
/// table entry -- callers are responsible for unmapping/no longer
/// translating through it first (see `vmm::free_process_page_table`,
/// the only current caller).
pub fn vmm_free_frame(addr: usize) {
    PHYSICAL_PMM.lock().free_frame(addr / PAGE_SIZE);
}

/// Convenience alias expected by the ELF loader (`crate::memory::alloc_frame`).
pub fn alloc_frame() -> Option<usize> {
    vmm_alloc_frame()
}

/// Maps a virtual address to a physical frame in the specified page table root.
///
/// x86_64-only in practice: its one caller (main.rs's framebuffer MMIO
/// identity-mapping) is itself x86_64-only, since FB_ADDR is the QEMU
/// 'pc' machine's fixed LFB address with no AArch64/Pi equivalent. The
/// real, general-purpose page mapper for both architectures is
/// `vmm::arch::map_page`; this one exists specifically for mapping a
/// *known physical* address (MMIO) into an *existing* root passed as
/// a raw `usize`, which is what the framebuffer setup needed.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))]
pub unsafe fn map_page(page_table_root: usize, vaddr: usize, paddr: usize) -> Result<(), &'static str> {
    #[cfg(target_arch = "x86_64")]
    {
        // `page_table_root` arrives as a raw physical CR3-style value
        // (that's what every caller naturally has on hand); translate
        // once here so every table pointer below is dereferenceable.
        let pml4 = phys_to_virt(page_table_root) as *mut u64;

        let pml4_idx = (vaddr >> 39) & 0x1FF;
        let pdpt_idx = (vaddr >> 30) & 0x1FF;
        let pd_idx   = (vaddr >> 21) & 0x1FF;
        let pt_idx   = (vaddr >> 12) & 0x1FF;
    
        // Returns a *dereferenceable* pointer to the next-level table
        // (already translated), not the raw physical address -- that
        // stays exactly where it belongs, stored in the entry itself.
        unsafe fn get_or_create_table(entry: *mut u64) -> Result<*mut u64, &'static str> {
            unsafe {
                let val = entry.read();
                if (val & 1) != 0 {
                    Ok(phys_to_virt((val & !0xFFF) as usize) as *mut u64)
                } else {
                    let new_frame = vmm_alloc_frame().ok_or("Out of memory: failed to allocate page table frame")?;
                    ptr::write_bytes(phys_to_virt(new_frame) as *mut u8, 0, PAGE_SIZE);
                    entry.write((new_frame as u64) | 0x7); // Present, Writable, User -- physical, on purpose
                    Ok(phys_to_virt(new_frame) as *mut u64)
                }
            }
        }

        unsafe {
            let pdpt = get_or_create_table(pml4.add(pml4_idx))?;
            let pd = get_or_create_table(pdpt.add(pdpt_idx))?;
            let pt = get_or_create_table(pd.add(pd_idx))?;

            pt.add(pt_idx).write((paddr as u64) | 0x7); // Present, Writable, User -- physical, on purpose

            core::arch::asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags));
        }

        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    {
        let _ = (page_table_root, vaddr, paddr);
        Err("map_page not implemented for AArch64")
    }
}


/// Protects boot and kernel memory from being allocated by the VMM.
///
/// `kernel_phys_start`/`kernel_phys_end` must be the kernel's own
/// *physical* extent. On x86_64 this can't be assumed -- Limine
/// picks its own physical placement for the loaded image (query it
/// via `limine::executable_address()`; Multiboot2 boots always land
/// at `KERNEL_LMA_BASE`/1MiB, see linker_x86.ld) -- passing a virtual
/// address here (`_kernel_end` on its own, unadjusted, is one) or
/// assuming a fixed start left the frame allocator free to hand out
/// the kernel's own running code/data, silently corrupting it the
/// moment anything called `vmm_alloc_frame()`. On aarch64, physical
/// == virtual (mmu.rs's permanent identity map), so `_kernel_end`
/// directly and `kernel_phys_start = 0` (reproducing this function's
/// previous, aarch64-only-correct behavior of just reserving
/// everything from address 0) are both fine as-is.
///
/// `heap_start`/`heap_size` should match whatever's passed to
/// `init_memory_subsystem`.
///
/// `ramdisk`, if the bootloader provided one (`limine::first_module()`
/// or its Multiboot2 equivalent), is the ramdisk's own physical
/// (address, size) -- without this, the frame allocator can hand out
/// the ramdisk's memory to something else, corrupting the mounted
/// filesystem's headers.
pub unsafe fn protect_boot_memory(
    kernel_phys_start: usize,
    kernel_phys_end: usize,
    heap_start: usize,
    heap_size: usize,
    ramdisk: Option<(usize, usize)>,
) {
    let mut pmm = PHYSICAL_PMM.lock();
    pmm.reserve_range(0, 256); // First 1MiB: BIOS/EBDA and other fixed low-memory structures.

    let kernel_start_frame = kernel_phys_start / PAGE_SIZE;
    let kernel_end_frame = (kernel_phys_end + PAGE_SIZE - 1) / PAGE_SIZE;
    if kernel_end_frame > kernel_start_frame {
        pmm.reserve_range(kernel_start_frame, kernel_end_frame - kernel_start_frame);
    }

    let heap_start_frame = heap_start / PAGE_SIZE;
    let heap_end_frame = (heap_start + heap_size + PAGE_SIZE - 1) / PAGE_SIZE;
    if heap_end_frame > heap_start_frame {
        pmm.reserve_range(heap_start_frame, heap_end_frame - heap_start_frame);
    }

    if let Some((ramdisk_addr, ramdisk_size)) = ramdisk {
        let ramdisk_start_frame = ramdisk_addr / PAGE_SIZE;
        let ramdisk_end_frame = (ramdisk_addr + ramdisk_size + PAGE_SIZE - 1) / PAGE_SIZE;
        if ramdisk_end_frame > ramdisk_start_frame {
            pmm.reserve_range(ramdisk_start_frame, ramdisk_end_frame - ramdisk_start_frame);
        }
    }
}

/// Tears down the temporary lower-half identity mapping (PML4[0]) now
/// that a real IDT is live.
///
/// This has to run *after* `interrupts::init()` (see the call site in
/// `kmain`, main.rs): before that, `lidt` has never run, so any fault
/// in this window can't be delivered, escalates to a double fault for
/// the same reason, and triple-faults the machine with no diagnostic
/// output. Running it here instead means a fault produces a clean
/// panic.
///
/// On a Multiboot2 boot this removes a real mapping --
/// boot_multiboot2.s's trampoline builds one to survive the
/// paging-enable transition. On a Limine boot it's a harmless no-op:
/// base revision 3 (what this kernel requests) doesn't put anything
/// at PML4[0] to begin with.
///
/// # Safety
/// Must only be called once, after paging is already active with the
/// bootloader's page tables (true at the point `kmain` calls this).
/// x86_64 only -- there is no lower-half identity map to remove on
/// the aarch64 boot path.
#[cfg(target_arch = "x86_64")]
pub unsafe fn unmap_low_half_identity_map() {
    unsafe {
        let cr3: usize;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        let root_phys = cr3 & !0xFFF;

        // PML4[0]. On Multiboot2, boot_multiboot2.s also mirrors this
        // exact physical range at PML4[256] (see its comments), which
        // is what phys_to_virt below actually resolves through --
        // *not* the mapping being zeroed here.
        let pml4_0 = phys_to_virt(root_phys) as *mut u64;
        core::ptr::write_volatile(pml4_0, 0);

        // Flush the TLB by reloading CR3.
        core::arch::asm!("mov cr3, {0}", in(reg) cr3);
    }
}

/// Explicit initialization entry point
pub unsafe fn init_memory_subsystem(heap_start: usize, heap_size: usize) {
    unsafe {
        HEAP_ALLOCATOR.lock().init(phys_to_virt(heap_start), heap_size);
    }
}

/// Creates a new, isolated page table for a user process.
pub unsafe fn create_process_page_table() -> Option<usize> {
    let root_frame = crate::memory::vmm_alloc_frame()?;
    
    unsafe {
        core::ptr::write_bytes(phys_to_virt(root_frame) as *mut u8, 0, 4096);
    }

    #[cfg(target_arch = "x86_64")]
    {
        let current_cr3: usize;
        unsafe {  
            core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
        }
        // `root_frame` itself stays physical -- it's returned as-is
        // below for storage in the caller's `memory_root` field, which
        // every other function in this file expects to hold a raw
        // physical CR3-style value. Only the pointers used to actually
        // walk/copy the tables here need translating.
        let active_root = phys_to_virt(current_cr3 & !0xFFF) as *const u64;
        let new_root = phys_to_virt(root_frame) as *mut u64;
        
        unsafe { 
            // The kernel is higher-half (see linker_x86.ld's
            // KERNEL_VMA, PML4 index 511): its code, data and any
            // runtime physical/MMIO mappings (e.g. the framebuffer
            // identity-map in main.rs) live there, not in the low
            // half. Index 0 also gets used transiently at boot
            // (Limine's own use of it, if any, or boot_multiboot2.s's
            // trampoline -- see unmap_low_half_identity_map), index
            // 256 by memory::HHDM_OFFSET's mapping, and nothing else
            // besides the kernel populates any other index at boot
            // time. So copying the *entire* 512-entry table here is
            // equivalent to hand-picking exactly the indices the
            // kernel actually uses today, and it keeps working
            // automatically if that set ever changes -- without this,
            // a freshly spawned process's page table would be missing
            // one of them, and the instant the scheduler switched CR3
            // to it, the very next instruction fetch or physical-memory
            // access through that missing mapping would have nowhere
            // to translate through and would fault immediately.
            //
            // Note: this shares the underlying page-table structures
            // with the parent, not just the mappings -- by itself that
            // would let two processes (or a process and the kernel)
            // step on each other by both extending the same shared PD
            // entry for their own private mappings. What actually
            // prevents that: every index this loop copies is either
            // populated only by the kernel/bootloader, or, for every
            // other index, is still completely empty at this point (nothing has ever mapped anything there) -- and
            // every process's *private* mappings are required to live
            // at memory::USER_SPACE_BASE or above (index 1, see its
            // doc comment and elf.rs's segment validation), which
            // starts out unpopulated in this copy. The first private
            // mapping any process makes forces its own fresh
            // PDPT/PD/PT chain there, not a walk into a shared one --
            // the sharing here is real but confined entirely to the
            // indices the kernel itself already owns.
            for i in 0..512 {
                new_root.add(i).write(active_root.add(i).read());
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // Same problem as the x86_64 branch above, same fix: without
        // this, the instant run_schedule switches TTBR0_EL1 to a
        // freshly spawned process's table, the very next kernel-side
        // instruction fetch (still at a low, kernel-identity-mapped
        // address) has nothing to translate through and faults
        // immediately. mmu.rs builds one flat identity map covering
        // kernel RAM + MMIO, entirely inside L0 index 0 (a 512GB
        // span) -- unlike x86_64's 512-entry PML4 where only the low
        // 256 of 512 entries are ever used, there's no "unused half"
        // to reason about here, so this just copies the whole 4KiB
        // root table verbatim rather than picking indices.
        //
        // Same aliasing note as x86_64, same resolution: L0 index 0
        // is shared (kernel-only, nothing process-private is allowed
        // there), index 1+ starts unpopulated in this copy, and
        // memory::USER_SPACE_BASE (index 1's start) is where every
        // process's own mappings are required to live -- see its doc
        // comment for the full reasoning.
        let kernel_root = crate::mmu::kernel_root();
        if kernel_root != 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    kernel_root as *const u8,
                    root_frame as *mut u8,
                    4096,
                );
            }
        }
    }
    
    Some(root_frame)
}
