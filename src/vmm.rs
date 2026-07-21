//! Virtual Memory Management for mitosOS.
//! Abstracts the translation of virtual addresses to physical frames.

use crate::memory::{vmm_alloc_frame, MapFlags};

/// Common Memory Errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    FrameAllocationFailed,
    AlreadyMapped,
    InvalidAddress,
}

#[cfg(target_arch = "x86_64")]
pub mod arch {
    use super::*;

    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct PageTableEntry(u64);

    impl PageTableEntry {
        pub fn is_present(&self) -> bool {
            (self.0 & 1) != 0
        }

        pub fn physical_address(&self) -> usize {
            (self.0 & 0x000F_FFFF_FFFF_F000) as usize
        }

        pub fn set_frame(&mut self, phys_addr: usize, flags: MapFlags) {
            let mut raw = (phys_addr & 0x000F_FFFF_FFFF_F000) | 1;
            if flags.writable {
                raw |= 1 << 1;
            }
            if flags.user_accessible {
                raw |= 1 << 2;
            }
            if flags.execute_disable {
                raw |= 1 << 63;
            }
            self.0 = raw;
        }
    }

    #[repr(align(4096))]
    pub struct PageTable {
        pub entries: [PageTableEntry; 512],
    }

    /// # Safety
    /// `root` must point to a valid, zeroed (or already-populated)
    /// `PageTable` that the caller owns exclusively for the duration
    /// of this call.
    pub unsafe fn map_page(
        root: *mut PageTable,
        virt: usize,
        phys: usize,
        flags: MapFlags,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 || phys & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let pml4_idx = (virt >> 39) & 0x1FF;
        let pdpt_idx = (virt >> 30) & 0x1FF;
        let pd_idx = (virt >> 21) & 0x1FF;
        let pt_idx = (virt >> 12) & 0x1FF;

        let pdpt = unsafe { next_table(&mut (*root).entries[pml4_idx])? };
        let pd = unsafe { next_table(&mut (*pdpt).entries[pdpt_idx])? };
        let pt = unsafe { next_table(&mut (*pd).entries[pd_idx])? };

        let entry = unsafe { &mut (*pt).entries[pt_idx] };
        if entry.is_present() {
            return Err(MemoryError::AlreadyMapped);
        }
        entry.set_frame(phys, flags);

        unsafe {
            core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        }
        Ok(())
    }

    /// # Safety
    /// `entry` must be a live entry inside a `PageTable` the caller owns
    /// exclusively.
    unsafe fn next_table(entry: &mut PageTableEntry) -> Result<*mut PageTable, MemoryError> {
        if !entry.is_present() {
            let frame = vmm_alloc_frame().ok_or(MemoryError::FrameAllocationFailed)?;
            unsafe {
                core::ptr::write_bytes(frame as *mut u8, 0, 4096);
            }
            entry.set_frame(
                frame,
                MapFlags {
                    writable: true,
                    user_accessible: true,
                    execute_disable: false,
                },
            );
        }
        Ok(entry.physical_address() as *mut PageTable)
    }
}

#[cfg(target_arch = "aarch64")]
pub mod arch {
    use super::*;

    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct PageTableEntry(u64);

    impl PageTableEntry {
        pub fn is_present(&self) -> bool {
            (self.0 & 1) != 0
        }

        pub fn physical_address(&self) -> usize {
            (self.0 & 0x0000_FFFF_FFFF_F000) as usize
        }

        pub fn set_frame(&mut self, phys: usize, flags: MapFlags, is_table: bool) {
            let mut raw = (phys & 0x0000_FFFF_FFFF_F000) | (if is_table { 3 } else { 1 });
            raw |= 1 << 10; // Access Flag
            if !flags.writable {
                raw |= 1 << 7;
            }
            if flags.user_accessible {
                raw |= 1 << 6;
            }
            if flags.execute_disable {
                raw |= (1 << 53) | (1 << 54);
            }
            self.0 = raw;
        }
    }

    #[repr(align(4096))]
    pub struct PageTable {
        pub entries: [PageTableEntry; 512],
    }

    /// # Safety
    /// `root` must point to a valid, zeroed (or already-populated)
    /// `PageTable` that the caller owns exclusively for the duration
    /// of this call.
    pub unsafe fn map_page(
        root: *mut PageTable,
        virt: usize,
        phys: usize,
        flags: MapFlags,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 || phys & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let l0_idx = (virt >> 39) & 0x1FF;
        let l1_idx = (virt >> 30) & 0x1FF;
        let l2_idx = (virt >> 21) & 0x1FF;
        let l3_idx = (virt >> 12) & 0x1FF;

        let l1 = unsafe { next_table(&mut (*root).entries[l0_idx])? };
        let l2 = unsafe { next_table(&mut (*l1).entries[l1_idx])? };
        let l3 = unsafe { next_table(&mut (*l2).entries[l2_idx])? };

        let entry = unsafe { &mut (*l3).entries[l3_idx] };
        if entry.is_present() {
            return Err(MemoryError::AlreadyMapped);
        }
        entry.set_frame(phys, flags, false);

        unsafe {
            core::arch::asm!(
                "tlbi vaae1is, {}",
                "dsb ish",
                "isb",
                in(reg) virt >> 12,
                options(nostack)
            );
        }
        Ok(())
    }

    /// # Safety
    /// `entry` must be a live entry inside a `PageTable` the caller owns
    /// exclusively.
    unsafe fn next_table(entry: &mut PageTableEntry) -> Result<*mut PageTable, MemoryError> {
        if !entry.is_present() {
            let frame = vmm_alloc_frame().ok_or(MemoryError::FrameAllocationFailed)?;
            unsafe {
                core::ptr::write_bytes(frame as *mut u8, 0, 4096);
            }
            entry.set_frame(
                frame,
                MapFlags {
                    writable: true,
                    user_accessible: true,
                    execute_disable: false,
                },
                true,
            );
        }
        Ok(entry.physical_address() as *mut PageTable)
    }
}
