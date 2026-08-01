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
            self.0 = raw as u64;
        }

        /// Ensures user-mode (Ring 3) permissions are set on parent table entries
        pub fn ensure_user_accessible(&mut self) {
            self.0 |= 1 << 2; // Set Bit 2 (U/S)
        }
    }

    #[repr(align(4096))]
    pub struct PageTable {
        pub entries: [PageTableEntry; 512],
    }

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
        let pd_idx   = (virt >> 21) & 0x1FF;
        let pt_idx   = (virt >> 12) & 0x1FF;

        // Pass `flags.user_accessible` down to ensure parent entries get the USER bit set
        let pdpt = unsafe { next_table(&mut (*root).entries[pml4_idx], flags.user_accessible)? };
        let pd   = unsafe { next_table(&mut (*pdpt).entries[pdpt_idx], flags.user_accessible)? };
        let pt   = unsafe { next_table(&mut (*pd).entries[pd_idx], flags.user_accessible)? };

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

    unsafe fn next_table(
        entry: &mut PageTableEntry,
        user_accessible: bool,
    ) -> Result<*mut PageTable, MemoryError> {
        if !entry.is_present() {
            let frame = vmm_alloc_frame().ok_or(MemoryError::FrameAllocationFailed)?;
            unsafe {
                core::ptr::write_bytes(frame as *mut u8, 0, 4096);
            }
            entry.set_frame(
                frame,
                MapFlags {
                    writable: true,
                    user_accessible: true, // New intermediate tables must be user-accessible
                    execute_disable: false,
                    device: false,
                },
            );
        } else if user_accessible {
            // CRITICAL: If the table already exists, upgrade its permissions so Ring 3 can traverse it!
            entry.ensure_user_accessible();
        }
        Ok(entry.physical_address() as *mut PageTable)
    }

        /// Unmaps a mapped virtual page, clearing the entry and flushing the TLB.
    pub unsafe fn unmap_page(
        root: *mut PageTable,
        virt: usize,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let pml4_idx = (virt >> 39) & 0x1FF;
        let pdpt_idx = (virt >> 30) & 0x1FF;
        let pd_idx   = (virt >> 21) & 0x1FF;
        let pt_idx   = (virt >> 12) & 0x1FF;

        let pml4_entry = &mut (*root).entries[pml4_idx];
        if !pml4_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let pdpt = pml4_entry.physical_address() as *mut PageTable;
        let pdpt_entry = &mut (*pdpt).entries[pdpt_idx];
        if !pdpt_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let pd = pdpt_entry.physical_address() as *mut PageTable;
        let pd_entry = &mut (*pd).entries[pd_idx];
        if !pd_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let pt = pd_entry.physical_address() as *mut PageTable;
        let pt_entry = &mut (*pt).entries[pt_idx];
        
        if !pt_entry.is_present() {
            return Err(MemoryError::InvalidAddress);
        }

        // Clear the entry
        pt_entry.0 = 0;

        // Invalidate the TLB cache for this specific virtual address
        unsafe {
            core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        }
        
        Ok(())
    }

    /// Translates a virtual address to its mapped physical address, if it exists.
    pub unsafe fn translate_addr(
        root: *const PageTable,
        virt: usize,
    ) -> Option<usize> {
        let pml4_idx = (virt >> 39) & 0x1FF;
        let pdpt_idx = (virt >> 30) & 0x1FF;
        let pd_idx   = (virt >> 21) & 0x1FF;
        let pt_idx   = (virt >> 12) & 0x1FF;
        let offset   = virt & 0xFFF;

        let pml4_entry = &(*root).entries[pml4_idx];
        if !pml4_entry.is_present() { return None; }

        let pdpt = pml4_entry.physical_address() as *const PageTable;
        let pdpt_entry = &(*pdpt).entries[pdpt_idx];
        if !pdpt_entry.is_present() { return None; }

        // Check for 1GB Huge Page at PDPT level
        if (pdpt_entry.0 & (1 << 7)) != 0 {
            return Some((pdpt_entry.physical_address() & !0x3FFF_FFFF) + (virt & 0x3FFF_FFFF));
        }

        let pd = pdpt_entry.physical_address() as *const PageTable;
        let pd_entry = &(*pd).entries[pd_idx];
        if !pd_entry.is_present() { return None; }

        // Check for 2MB Huge Page at PD level
        if (pd_entry.0 & (1 << 7)) != 0 {
            return Some((pd_entry.physical_address() & !0x1F_FFFF) + (virt & 0x1F_FFFF));
        }

        let pt = pd_entry.physical_address() as *const PageTable;
        let pt_entry = &(*pt).entries[pt_idx];
        if !pt_entry.is_present() { return None; }

        Some(pt_entry.physical_address() + offset)
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
            // Bits[1:0] = 0b11 in *both* branches despite the name --
            // architecturally that's a Table descriptor at L0-L2 and a
            // Page descriptor at L3, and those two happen to share the
            // identical encoding; only which level's walk produced the
            // entry tells the hardware which one it is. This 4-level
            // walk (map_page below always descends all the way to L3)
            // never emits the *other* legal encoding, 0b01 -- a Block
            // descriptor, valid only at L1/L2 -- so that value was
            // never correct here. At L3 specifically, 0b01 isn't a
            // Block descriptor either; it's just an invalid entry, so
            // every leaf this used to produce faulted the instant the
            // MMU was switched on in mmu.rs -- before that fault could
            // even reach the exception handler, since the stack and
            // the UART MMIO it needs to report on were themselves
            // behind the same broken encoding.
            let mut raw = (phys & 0x0000_FFFF_FFFF_F000) | 0b11;
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
            // AttrIndx[2:0] (bits 4:2) selects into MAIR_EL1 -- see
            // mmu.rs. Index 1 = Device-nGnRnE, index 0 = Normal
            // Write-Back. Table descriptors don't have this field
            // (those bits mean something else there: PXNTable/
            // APTable/etc at 59+, not attributes), so this only
            // applies to leaf (page) entries.
            if !is_table && flags.device {
                raw |= 1 << 2;
            }
            self.0 = raw as u64;
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
                    device: false,
                },
                true,
            );
        }
        Ok(entry.physical_address() as *mut PageTable)
    }

        /// Unmaps a mapped virtual page, clearing the entry and executing a rigorous TLB flush.
    pub unsafe fn unmap_page(
        root: *mut PageTable,
        virt: usize,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let l0_idx = (virt >> 39) & 0x1FF;
        let l1_idx = (virt >> 30) & 0x1FF;
        let l2_idx = (virt >> 21) & 0x1FF;
        let l3_idx = (virt >> 12) & 0x1FF;

        let l0_entry = &mut (*root).entries[l0_idx];
        if !l0_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let l1 = l0_entry.physical_address() as *mut PageTable;
        let l1_entry = &mut (*l1).entries[l1_idx];
        if !l1_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let l2 = l1_entry.physical_address() as *mut PageTable;
        let l2_entry = &mut (*l2).entries[l2_idx];
        if !l2_entry.is_present() { return Err(MemoryError::InvalidAddress); }

        let l3 = l2_entry.physical_address() as *mut PageTable;
        let l3_entry = &mut (*l3).entries[l3_idx];
        
        if !l3_entry.is_present() {
            return Err(MemoryError::InvalidAddress);
        }

        // Clear the entry (0 makes it an invalid descriptor)
        l3_entry.0 = 0;

        // Rigorous ARM TLB Invalidation & memory barrier synchronization
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

    /// Translates a virtual address to its mapped physical address, if it exists.
    pub unsafe fn translate_addr(
        root: *const PageTable,
        virt: usize,
    ) -> Option<usize> {
        let l0_idx = (virt >> 39) & 0x1FF;
        let l1_idx = (virt >> 30) & 0x1FF;
        let l2_idx = (virt >> 21) & 0x1FF;
        let l3_idx = (virt >> 12) & 0x1FF;
        let offset = virt & 0xFFF;

        let l0_entry = &(*root).entries[l0_idx];
        if !l0_entry.is_present() { return None; }

        let l1 = l0_entry.physical_address() as *const PageTable;
        let l1_entry = &(*l1).entries[l1_idx];
        if !l1_entry.is_present() { return None; }

        // Check if L1 is a block descriptor (1GB) instead of a table descriptor
        if (l1_entry.0 & 0b11) == 0b01 {
            return Some((l1_entry.physical_address() & !0x3FFF_FFFF) + (virt & 0x3FFF_FFFF));
        }

        let l2 = l1_entry.physical_address() as *const PageTable;
        let l2_entry = &(*l2).entries[l2_idx];
        if !l2_entry.is_present() { return None; }

        // Check if L2 is a block descriptor (2MB) instead of a table descriptor
        if (l2_entry.0 & 0b11) == 0b01 {
            return Some((l2_entry.physical_address() & !0x1F_FFFF) + (virt & 0x1F_FFFF));
        }

        let l3 = l2_entry.physical_address() as *const PageTable;
        let l3_entry = &(*l3).entries[l3_idx];
        if !l3_entry.is_present() { return None; }

        Some(l3_entry.physical_address() + offset)
    }

}


/// Attempts to resolve a memory fault via Demand Paging.
/// Returns `true` if a page was mapped and the instruction should be retried.
/// Returns `false` if it is a fatal violation (e.g., segmentation fault).
pub fn handle_page_fault(fault_addr: usize, is_present: bool, is_user: bool) -> bool {
    // If the page is already present, this is a protection/permissions violation
    // (like writing to a read-only page or Ring 3 accessing Ring 0 memory).
    // Since we do not have Copy-on-Write (CoW) yet, this is an instant SegFault.
    if is_present {
        return false;
    }

    // Align the faulting virtual address down to the nearest 4KB page boundary
    let aligned_virt = fault_addr & !0xFFF;

    // 1. Get the current active page table root directly from hardware
    #[cfg(target_arch = "x86_64")]
    let root = {
        let cr3: usize;
        unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, nomem)); }
        (cr3 & 0x000F_FFFF_FFFF_F000) as *mut arch::PageTable
    };

    #[cfg(target_arch = "aarch64")]
    let root = {
        let ttbr: usize;
        // AArch64 splits address spaces: TTBR0 for user, TTBR1 for kernel
        if fault_addr < 0xFFFF_0000_0000_0000 {
            unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) ttbr, options(nostack, nomem)); }
        } else {
            unsafe { core::arch::asm!("mrs {}, ttbr1_el1", out(reg) ttbr, options(nostack, nomem)); }
        }
        (ttbr & 0x0000_FFFF_FFFF_F000) as *mut arch::PageTable
    };

    // 2. Allocate a fresh physical frame
    let frame = match crate::memory::vmm_alloc_frame() {
        Some(f) => f,
        None => return false, // OOM: System is completely out of physical memory
    };

    // 3. Zero out the memory to prevent leaking old data to new processes
    unsafe {
        core::ptr::write_bytes(frame as *mut u8, 0, 4096);
    }

    // 4. Map it in dynamically
    let flags = crate::memory::MapFlags {
        writable: true, // Default to writable for new demand-paged memory
        user_accessible: is_user,
        execute_disable: false,
        device: false,
    };

    unsafe {
        arch::map_page(root, aligned_virt, frame, flags).is_ok()
    }
}
