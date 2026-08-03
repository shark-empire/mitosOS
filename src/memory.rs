//! Professional Production-Ready Memory Subsystem for mitosOS.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

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
        alloc_start as *mut u8
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
        let pml4 = page_table_root as *mut u64;
        
        let pml4_idx = (vaddr >> 39) & 0x1FF;
        let pdpt_idx = (vaddr >> 30) & 0x1FF;
        let pd_idx   = (vaddr >> 21) & 0x1FF;
        let pt_idx   = (vaddr >> 12) & 0x1FF;
    
        unsafe fn get_or_create_table(entry: *mut u64) -> Result<*mut u64, &'static str> {
            unsafe {
                let val = entry.read();
                if (val & 1) != 0 {
                    Ok(((val & !0xFFF) as usize) as *mut u64)
                } else {
                    let new_frame = vmm_alloc_frame().ok_or("Out of memory: failed to allocate page table frame")?;
                    ptr::write_bytes(new_frame as *mut u8, 0, PAGE_SIZE);
                    entry.write((new_frame as u64) | 0x7); // Present, Writable, User
                    Ok(new_frame as *mut u64)
                }
            }
        }

        unsafe {
            let pdpt = get_or_create_table(pml4.add(pml4_idx))?;
            let pd = get_or_create_table(pdpt.add(pdpt_idx))?;
            let pt = get_or_create_table(pd.add(pd_idx))?;

            pt.add(pt_idx).write((paddr as u64) | 0x7); // Present, Writable, User

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
/// `kernel_end_addr` should be the *real* end of the kernel's own image
/// (e.g. the linker-provided `_kernel_end` symbol), not a guess -- this
/// used to be called with a hardcoded `0x100000` placeholder, which is
/// actually where the kernel *starts*. That left the frame allocator
/// free to hand out the very first physical frame it owns (the kernel's
/// own code/data), silently corrupting the running kernel the moment
/// anything called `vmm_alloc_frame()` for a new page table, an ELF
/// segment, a DMA buffer, etc.
///
/// `heap_start`/`heap_size` should match whatever's passed to
/// `init_memory_subsystem` -- the heap lives at a fixed physical range
/// too (this kernel has no higher-half split, so physical == virtual
/// here), and the frame allocator needs to know to stay out of it for
/// the same reason.
pub unsafe fn protect_boot_memory(kernel_end_addr: usize, heap_start: usize, heap_size: usize) {
    let mut pmm = PHYSICAL_PMM.lock();
    pmm.reserve_range(0, 256); // Reserve first 1MB (BIOS/Stage1/Stage2)

    let kernel_end_frame = (kernel_end_addr + 4095) / 4096;
    if kernel_end_frame > 256 {
        pmm.reserve_range(256, kernel_end_frame - 256);
    }

    let heap_start_frame = heap_start / PAGE_SIZE;
    let heap_end_frame = (heap_start + heap_size + PAGE_SIZE - 1) / PAGE_SIZE;
    if heap_end_frame > heap_start_frame {
        pmm.reserve_range(heap_start_frame, heap_end_frame - heap_start_frame);
    }

    // UPGRADE: Protect the Ramdisk loaded by stage2.s at 2MB (0x200000)
    // Stage2 loads up to 128KB of ramdisk data (256 sectors).
    // Without this, the physical frame allocator hands out the ramdisk 
    // memory to other processes, zeroing out the TAR filesystem headers.
    let ramdisk_start_frame = 0x200000 / PAGE_SIZE;
    let ramdisk_frames = 256 * 512 / PAGE_SIZE;
    pmm.reserve_range(ramdisk_start_frame, ramdisk_frames);
}

/// Explicit initialization entry point
pub unsafe fn init_memory_subsystem(heap_start: usize, heap_size: usize) {
    unsafe {
        HEAP_ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

/// Creates a new, isolated page table for a user process.
pub unsafe fn create_process_page_table() -> Option<usize> {
    let root_frame = crate::memory::vmm_alloc_frame()?;
    
    unsafe {
        core::ptr::write_bytes(root_frame as *mut u8, 0, 4096);
    }

    #[cfg(target_arch = "x86_64")]
    {
        let current_cr3: usize;
        unsafe {  
            core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
        }
        let active_root = (current_cr3 & !0xFFF) as *const u64;
        let new_root = root_frame as *mut u64;
        
        unsafe { 
            // This kernel has no higher-half split: it's identity-mapped
            // starting at 0x100000, and ELF segments load around
            // 0x400000+ -- everything lives in the LOW half of the
            // address space (PML4 entries 0..256), not 256..512. This
            // used to copy 256..512, which is empty here, leaving a
            // freshly spawned process's page table with *nothing* mapped
            // -- the instant the scheduler switched CR3 to it, the very
            // next instruction fetch (still running kernel code, at a
            // low-half address) had nowhere to translate through and
            // would fault immediately.
            //
            // Note: this shares the underlying page-table structures
            // with the parent, not just the mappings -- by itself that
            // would let two processes (or a process and the kernel)
            // step on each other by both extending the same shared PD
            // entry for their own private mappings. What actually
            // prevents that: PML4 entries 0..256 only ever have entry 0
            // populated (everything the kernel maps -- identity range,
            // MMIO -- fits inside the first 512GB), and every process's
            // *private* mappings are required to live at
            // memory::USER_SPACE_BASE or above (entry 1+, see its doc
            // comment and elf.rs's segment validation), which starts
            // out unpopulated in this copy. The first private mapping
            // any process makes forces its own fresh PDPT/PD/PT chain,
            // not a walk into the shared one -- the sharing here is
            // real but confined entirely to entry 0, which nothing
            // process-private is allowed to touch.
            for i in 0..256 {
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
