#![no_std]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// =========================================================================
// 1. PHYSICAL MEMORY MANAGER (Your Bitmap Allocator + Fast Intrinsic Search)
// =========================================================================

const PAGE_SIZE: usize = 4096;

pub struct BitmapAllocator<const N: usize> {
    bitmap: [u64; N],
}

impl<const N: usize> BitmapAllocator<N> {
    pub const fn new() -> Self {
        Self { bitmap: [0; N] }
    }

    /// Mark a specific frame as used
    pub fn allocate_frame(&mut self, frame_index: usize) -> bool {
        let array_idx = frame_index / 64;
        let bit_idx = frame_index % 64;

        if array_idx < N && (self.bitmap[array_idx] & (1 << bit_idx)) == 0 {
            self.bitmap[array_idx] |= 1 << bit_idx;
            true
        } else {
            false
        }
    }

    /// Mark a frame as free
    pub fn free_frame(&mut self, frame_index: usize) {
        let array_idx = frame_index / 64;
        let bit_idx = frame_index % 64;

        if array_idx < N {
            self.bitmap[array_idx] &= !(1 << bit_idx);
        }
    }

    /// High-speed scan using hardware bit-counting
    pub fn allocate_next_frame(&mut self) -> Option<usize> {
        for array_idx in 0..N {
            if self.bitmap[array_idx] != !0 {
                let bit_idx = (!self.bitmap[array_idx]).trailing_zeros() as usize;
                let frame_index = (array_idx * 64) + bit_idx;
                self.bitmap[array_idx] |= 1 << bit_idx;
                return Some(frame_index);
            }
        }
        None
    }
}

// =========================================================================
// 2. ULTRA-LIGHTWEIGHT SPINLOCK (Low Overhead Synchronization)
// =========================================================================

pub struct Mutex<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(user_data: T) -> Self {
        Mutex {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(user_data),
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

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {
        self.mutex.lock.store(false, Ordering::Release);
    }
}

// =========================================================================
// 3. O(1) CONSTANT-TIME HEAP ALLOCATOR (Power-of-Two Segregated Free List)
// =========================================================================

struct ListNode {
    next: *mut ListNode,
}

// 9 buckets tracking powers of two: 8, 16, 32, 64, 128, 256, 512, 1024, 2048
const BUCKET_COUNT: usize = 9;

pub struct FastBlockAllocator {
    buckets: [*mut ListNode; BUCKET_COUNT],
    heap_start: usize,
    heap_end: usize,
    next_free_byte: usize,
}

impl FastBlockAllocator {
    pub const fn new() -> Self {
        FastBlockAllocator {
            buckets: [ptr::null_mut(); BUCKET_COUNT],
            heap_start: 0,
            heap_end: 0,
            next_free_byte: 0,
        }
    }

    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;
        self.next_free_byte = heap_start;
    }

    #[inline(always)]
    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        let alloc_start = (self.next_free_byte + layout.align() - 1) & !(layout.align() - 1);
        let alloc_end = match alloc_start.checked_add(layout.size()) {
            Some(end) => end,
            None => return ptr::null_mut(),
        };

        if alloc_end > self.heap_end {
            ptr::null_mut() // Out of memory
        } else {
            self.next_free_byte = alloc_end;
            alloc_start as *mut u8
        }
    }
}

/// True O(1) bucket mapping using CPU hardware instructions via `trailing_zeros()`
#[inline(always)]
fn target_bucket_index(layout: &Layout) -> Option<usize> {
    let required_size = layout.size().max(layout.align());
    if required_size > 2048 {
        return None; // Too big for standard buckets, go to fallback
    }
    
    // Round up to the next power of two
    let size = required_size.next_power_of_two();
    if size <= 8 {
        Some(0) // Index 0 handles 8 bytes
    } else {
        // maps 16->1, 32->2, 64->3, 128->4, 256->5, 512->6, 1024->7, 2048->8
        Some((size.trailing_zeros() as usize) - 3)
    }
}

#[global_allocator]
static HEAP_ALLOCATOR: Mutex<FastBlockAllocator> = Mutex::new(FastBlockAllocator::new());

unsafe impl GlobalAlloc for Mutex<FastBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut allocator = self.lock();
        match target_bucket_index(&layout) {
            Some(index) => {
                let head = allocator.buckets[index];
                if !head.is_null() {
                    // O(1) Fast Path: Pop memory immediately from the linked block cell
                    allocator.buckets[index] = (*head).next;
                    head as *mut u8
                } else {
                    // Bucket is empty, carve out a fresh block from raw heap arena space
                    let block_size = if index == 0 { 8 } else { 1 << (index + 3) };
                    let new_layout = Layout::from_size_align(block_size, block_size).unwrap();
                    allocator.fallback_alloc(new_layout)
                }
            }
            None => allocator.fallback_alloc(layout), // Request > 2048 bytes
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut allocator = self.lock();
        match target_bucket_index(&layout) {
            Some(index) => {
                // O(1) Fast Release: Push the freed pointer directly back onto the stack bucket
                let new_node = ptr as *mut ListNode;
                (*new_node).next = allocator.buckets[index];
                allocator.buckets[index] = new_node;
            }
            None => {
                // Large blocks handled via bump allocation are reclaimed on direct system sweeps
            }
        }
    }
}

pub unsafe fn init_memory_subsystem(heap_start: usize, heap_size: usize) {
    HEAP_ALLOCATOR.lock().init(heap_start, heap_size);
}

// Statically track up to 256MB of physical RAM via your ultra-low footprint bitmap
pub static PHYSICAL_PMM: Mutex<BitmapAllocator<1024>> = Mutex::new(BitmapAllocator::new());

#[alloc_error_handler]
fn alloc_error_handler(_layout: core::alloc::Layout) -> ! {
    loop {}
}
