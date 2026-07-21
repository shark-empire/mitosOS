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
}

impl MapFlags {
    pub const fn kernel_code() -> Self {
        Self { writable: false, user_accessible: false, execute_disable: false }
    }

    pub const fn kernel_data() -> Self {
        Self { writable: true, user_accessible: false, execute_disable: true }
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

    pub fn reserve_range(&mut self, start_frame: usize, count: usize) {
        for i in 0..count { self.reserve_frame(start_frame + i); }
    }
}

pub static PHYSICAL_PMM: Mutex<BitmapAllocator<1024>> = Mutex::new(BitmapAllocator::new());

/// Bridge for the VMM
pub fn vmm_alloc_frame() -> Option<usize> {
    PHYSICAL_PMM.lock().allocate_next_frame().map(|idx| idx * PAGE_SIZE)
}

/// Protects boot and kernel memory from being allocated by the VMM
pub unsafe fn protect_boot_memory(kernel_end_addr: usize) {
    let mut pmm = PHYSICAL_PMM.lock();
    pmm.reserve_range(0, 256); // Reserve first 1MB (BIOS/Stage1/Stage2)
    let kernel_end_frame = (kernel_end_addr + 4095) / 4096;
    if kernel_end_frame > 256 {
        pmm.reserve_range(256, kernel_end_frame - 256);
    }
}

/// Explicit initialization entry point
pub unsafe fn init_memory_subsystem(heap_start: usize, heap_size: usize) {
    // Explicit unsafe block required by Rust 2024 edition rules 
    // even though the surrounding function is marked `unsafe`.
    unsafe {
        HEAP_ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

/// Creates a new, isolated page table for a user process.
/// It allocates a fresh root frame and copies the kernel's mappings 
/// into it so the kernel remains accessible during system calls.
pub unsafe fn create_process_page_table() -> Option<usize> {
    // 1. Allocate a fresh physical frame for the new root table
    let root_frame = crate::memory::vmm_alloc_frame()?;
    
    // 2. Zero out the entire frame to ensure no garbage mappings exist
    unsafe {
    core::ptr::write_bytes(root_frame as *mut u8, 0, 4096);
      }
    // 3. Copy the kernel's mappings into the new table.
    // On x86_64, the kernel typically lives in the top half of memory.
    // The top half of the PML4 table is the last 256 entries (out of 512).
    // (For AArch64 using TTBR1_EL1 for kernel and TTBR0_EL0 for user, 
    // you just allocate a fresh TTBR0 and don't need to copy).
    #[cfg(target_arch = "x86_64")]
    {
        let current_cr3: usize;
      unsafe {  
        core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
           }
        let active_root = (current_cr3 & !0xFFF) as *const u64;
        let new_root = root_frame as *mut u64;
        
        // Copy the upper 256 entries (kernel space) from the active table
       unsafe { 
        for i in 256..512 {
            new_root.add(i).write(active_root.add(i).read());
                 }
        }
    
    }
    
    Some(root_frame)
}

