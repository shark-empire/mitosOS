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

#[cfg(target_arch = "x86_64")]
pub static HHDM_OFFSET: AtomicUsize = AtomicUsize::new(0xFFFF_8000_0000_0000);

#[cfg(target_arch = "x86_64")]
pub fn set_hhdm_offset(offset: usize) {
    HHDM_OFFSET.store(offset, Ordering::SeqCst);
}

#[cfg(target_arch = "x86_64")]
pub fn hhdm_offset() -> usize {
    HHDM_OFFSET.load(Ordering::SeqCst)
}

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

// Limine Memory Map Entry Types
pub const LIMINE_MEMMAP_USABLE: u64 = 0;
pub const LIMINE_MEMMAP_RESERVED: u64 = 1;
pub const LIMINE_MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const LIMINE_MEMMAP_ACPI_NVS: u64 = 3;
pub const LIMINE_MEMMAP_BAD_MEMORY: u64 = 4;
pub const LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const LIMINE_MEMMAP_EXECUTABLE_AND_MODULES: u64 = 6;
pub const LIMINE_MEMMAP_FRAMEBUFFER: u64 = 7;

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
    /// Starts with ALL physical frames marked as reserved (`!0u64`). Memory must
    /// be explicitly freed into the pool after parsing bootloader maps.
    pub const fn new() -> Self {
        Self { bitmap: [!0u64; N] }
    }

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

    pub fn free_frame(&mut self, frame_index: usize) {
        if frame_index == 0 {
            return;
        }
        let array_idx = frame_index / 64;
        let bit_idx = frame_index % 64;
        if array_idx < N { self.bitmap[array_idx] &= !(1 << bit_idx); }
    }

    pub fn reserve_range(&mut self, start_frame: usize, count: usize) {
        for i in 0..count { self.reserve_frame(start_frame + i); }
    }

    pub fn free_range(&mut self, start_frame: usize, count: usize) {
        for i in 0..count { self.free_frame(start_frame + i); }
    }
}

pub static PHYSICAL_PMM: Mutex<BitmapAllocator<1024>> = Mutex::new(BitmapAllocator::new());
pub const USER_SPACE_BASE: usize = 1 << 39;

pub fn vmm_alloc_frame() -> Option<usize> {
    PHYSICAL_PMM.lock().allocate_next_frame().map(|idx| idx * PAGE_SIZE)
}

pub fn vmm_free_frame(addr: usize) {
    PHYSICAL_PMM.lock().free_frame(addr / PAGE_SIZE);
}

pub fn alloc_frame() -> Option<usize> {
    vmm_alloc_frame()
}

/// Parses Limine's memory map response and frees only USABLE regions into the PMM.
/// Always enforces a strict reservation on physical frames 0..256 (0x0 - 0x100000).
pub fn init_pmm_from_limine() -> Result<(), &'static str> {
    let memmap = crate::limine::memmap().ok_or("Limine memory map response unavailable")?;
    let mut pmm = PHYSICAL_PMM.lock();

    for entry in memmap.entries() {
        if entry.typ == LIMINE_MEMMAP_USABLE {
            let start_frame = (entry.base as usize) / PAGE_SIZE;
            let count = (entry.length as usize) / PAGE_SIZE;
            pmm.free_range(start_frame, count);
        }
    }

    // Permanently reserve physical [0x0, 0x100000) (Lower 1 MiB) for BIOS/EBDA/Interrupt tables.
    pmm.reserve_range(0, 256);

    Ok(())
}

/// Reclaims ACPI reclaimable and bootloader reclaimable memory regions into the
/// PMM pool. Call this AFTER `hal::acpi::init()` finishes parsing tables.
pub fn reclaim_boot_memory() {
    if let Some(memmap) = crate::limine::memmap() {
        let mut pmm = PHYSICAL_PMM.lock();
        for entry in memmap.entries() {
            if entry.typ == LIMINE_MEMMAP_ACPI_RECLAIMABLE 
                || entry.typ == LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE 
            {
                let start_frame = (entry.base as usize) / PAGE_SIZE;
                let count = (entry.length as usize) / PAGE_SIZE;
                pmm.free_range(start_frame, count);
            }
        }
        // Protect lower 1 MiB under all circumstances
        pmm.reserve_range(0, 256);
    }
}

pub unsafe fn protect_boot_memory(
    kernel_phys_start: usize,
    kernel_phys_end: usize,
    heap_start: usize,
    heap_size: usize,
    ramdisk: Option<(usize, usize)>,
) {
    let mut pmm = PHYSICAL_PMM.lock();
    pmm.reserve_range(0, 256);

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

#[cfg_attr(target_arch = "aarch64", allow(dead_code))]
pub unsafe fn map_page(page_table_root: usize, vaddr: usize, paddr: usize) -> Result<(), &'static str> {
    #[cfg(target_arch = "x86_64")]
    {
        let pml4 = phys_to_virt(page_table_root) as *mut u64;

        let pml4_idx = (vaddr >> 39) & 0x1FF;
        let pdpt_idx = (vaddr >> 30) & 0x1FF;
        let pd_idx   = (vaddr >> 21) & 0x1FF;
        let pt_idx   = (vaddr >> 12) & 0x1FF;
    
        unsafe fn get_or_create_table(entry: *mut u64) -> Result<*mut u64, &'static str> {
            unsafe {
                let val = entry.read();
                if (val & 1) != 0 {
                    Ok(phys_to_virt((val & !0xFFF) as usize) as *mut u64)
                } else {
                    let new_frame = vmm_alloc_frame().ok_or("Out of memory: failed to allocate page table frame")?;
                    ptr::write_bytes(phys_to_virt(new_frame) as *mut u8, 0, PAGE_SIZE);
                    entry.write((new_frame as u64) | 0x7);
                    Ok(phys_to_virt(new_frame) as *mut u64)
                }
            }
        }

        unsafe {
            let pdpt = get_or_create_table(pml4.add(pml4_idx))?;
            let pd = get_or_create_table(pdpt.add(pdpt_idx))?;
            let pt = get_or_create_table(pd.add(pd_idx))?;

            pt.add(pt_idx).write((paddr as u64) | 0x7);
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

#[cfg(target_arch = "x86_64")]
pub unsafe fn unmap_low_half_identity_map() {
    unsafe {
        let cr3: usize;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        let root_phys = cr3 & !0xFFF;

        let pml4_0 = phys_to_virt(root_phys) as *mut u64;
        core::ptr::write_volatile(pml4_0, 0);

        core::arch::asm!("mov cr3, {0}", in(reg) cr3);
    }
}

pub unsafe fn init_memory_subsystem(heap_start: usize, heap_size: usize) {
    unsafe {
        HEAP_ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

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
        let active_root = phys_to_virt(current_cr3 & !0xFFF) as *const u64;
        let new_root = phys_to_virt(root_frame) as *mut u64;
        
        unsafe { 
            for i in 0..512 {
                new_root.add(i).write(active_root.add(i).read());
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
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
