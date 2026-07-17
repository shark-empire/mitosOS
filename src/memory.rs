//! Physical Memory Manager - Bitmap Allocator
//! Efficiently tracks RAM without needing a complex heap.

const PAGE_SIZE: usize = 4096;

pub struct BitmapAllocator<const N: usize> {
    bitmap: [u64; N],
}

impl<const N: usize> BitmapAllocator<N> {
    pub const fn new() -> Self {
        Self { bitmap: [0; N] }
    }

    /// Mark a frame as used
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
}
