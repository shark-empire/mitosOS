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

        /// Bit 2 (U/S). Used by the syscall-layer pointer validator to
        /// confirm a ring-3-supplied address is actually inside that
        /// process's own mapped memory before the kernel touches it.
        pub fn is_user_accessible(&self) -> bool {
            (self.0 & (1 << 2)) != 0
        }

        /// Bit 1 (R/W).
        pub fn is_writable(&self) -> bool {
            (self.0 & (1 << 1)) != 0
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

    /// `root` is a raw physical page-table-root address (straight off
    /// CR3, or a stored `memory_root` field) -- that's what every
    /// caller naturally has on hand. Translated to a dereferenceable
    /// pointer once, right here; see `memory::phys_to_virt`'s doc
    /// comment for why that's necessary at all on this architecture.
    pub unsafe fn map_page(
        root: *mut PageTable,
        virt: usize,
        phys: usize,
        flags: MapFlags,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 || phys & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let root = crate::memory::phys_to_virt(root as usize) as *mut PageTable;

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
                core::ptr::write_bytes(crate::memory::phys_to_virt(frame) as *mut u8, 0, 4096);
            }
            entry.set_frame(
                frame, // physical, on purpose -- this is what a page-table entry stores
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
        // Dereferenceable pointer, not the raw physical address.
        Ok(crate::memory::phys_to_virt(entry.physical_address()) as *mut PageTable)
    }

        /// Unmaps a mapped virtual page, clearing the entry and flushing the TLB.
    pub unsafe fn unmap_page(
        root: *mut PageTable,
        virt: usize,
    ) -> Result<(), MemoryError> {
        if virt & 0xFFF != 0 {
            return Err(MemoryError::InvalidAddress);
        }

        let root = crate::memory::phys_to_virt(root as usize) as *mut PageTable;

        let pml4_idx = (virt >> 39) & 0x1FF;
        let pdpt_idx = (virt >> 30) & 0x1FF;
        let pd_idx   = (virt >> 21) & 0x1FF;
        let pt_idx   = (virt >> 12) & 0x1FF;

        unsafe {
            let pml4_entry = &mut (*root).entries[pml4_idx];
            if !pml4_entry.is_present() { return Err(MemoryError::InvalidAddress); }

            let pdpt = crate::memory::phys_to_virt(pml4_entry.physical_address()) as *mut PageTable;
            let pdpt_entry = &mut (*pdpt).entries[pdpt_idx];
            if !pdpt_entry.is_present() { return Err(MemoryError::InvalidAddress); }

            let pd = crate::memory::phys_to_virt(pdpt_entry.physical_address()) as *mut PageTable;
            let pd_entry = &mut (*pd).entries[pd_idx];
            if !pd_entry.is_present() { return Err(MemoryError::InvalidAddress); }

            let pt = crate::memory::phys_to_virt(pd_entry.physical_address()) as *mut PageTable;
            let pt_entry = &mut (*pt).entries[pt_idx];

            if !pt_entry.is_present() {
                return Err(MemoryError::InvalidAddress);
            }

            // Clear the entry
            pt_entry.0 = 0;
        }

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
        let root = crate::memory::phys_to_virt(root as usize) as *const PageTable;

        let pml4_idx = (virt >> 39) & 0x1FF;
        let pdpt_idx = (virt >> 30) & 0x1FF;
        let pd_idx   = (virt >> 21) & 0x1FF;
        let pt_idx   = (virt >> 12) & 0x1FF;
        let offset   = virt & 0xFFF;

        unsafe {
            let pml4_entry = &(*root).entries[pml4_idx];
            if !pml4_entry.is_present() { return None; }

            let pdpt = crate::memory::phys_to_virt(pml4_entry.physical_address()) as *const PageTable;
            let pdpt_entry = &(*pdpt).entries[pdpt_idx];
            if !pdpt_entry.is_present() { return None; }

            // Check for 1GB Huge Page at PDPT level. Returned as-is: the
            // whole point of this function is to hand back a *physical*
            // address, this one just isn't page-table-entry-granular.
            if (pdpt_entry.0 & (1 << 7)) != 0 {
                return Some((pdpt_entry.physical_address() & !0x3FFF_FFFF) + (virt & 0x3FFF_FFFF));
            }

            let pd = crate::memory::phys_to_virt(pdpt_entry.physical_address()) as *const PageTable;
            let pd_entry = &(*pd).entries[pd_idx];
            if !pd_entry.is_present() { return None; }

            // Check for 2MB Huge Page at PD level
            if (pd_entry.0 & (1 << 7)) != 0 {
                return Some((pd_entry.physical_address() & !0x1F_FFFF) + (virt & 0x1F_FFFF));
            }

            let pt = crate::memory::phys_to_virt(pd_entry.physical_address()) as *const PageTable;
            let pt_entry = &(*pt).entries[pt_idx];
            if !pt_entry.is_present() { return None; }

            Some(pt_entry.physical_address() + offset)
        }
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

        /// AP[1] (bit 6) -- see `set_frame` above, which sets this same
        /// bit for `flags.user_accessible`.
        pub fn is_user_accessible(&self) -> bool {
            (self.0 & (1 << 6)) != 0
        }

        /// AP[2] (bit 7) is the *read-only* bit -- `set_frame` sets it
        /// when `!flags.writable`, so writable is this bit being clear.
        pub fn is_writable(&self) -> bool {
            (self.0 & (1 << 7)) == 0
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

        unsafe {
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
        }

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

        unsafe {
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

    // 3. Zero out the memory to prevent leaking old data to new processes.
    // `phys_to_virt` is a no-op on aarch64 (permanent identity map) and
    // the necessary higher-half translation on x86_64 (see its doc
    // comment), so this one line is correct on both architectures.
    unsafe {
        core::ptr::write_bytes(crate::memory::phys_to_virt(frame) as *mut u8, 0, 4096);
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

// =========================================================================
// Process address-space teardown
// =========================================================================

/// Frees every physical frame reachable from `root`'s user-space region,
/// then frees `root` itself.
///
/// `map_page` always walks a full 4 levels for a freshly-mapped page --
/// this kernel never emits a huge/block leaf itself, though
/// `translate_addr`/`handle_page_fault` above know how to read one if
/// something else ever put one there. Assuming a fixed-depth structure
/// below `root` (root -> L1 -> L2 -> L3, with L3's own entries as the
/// actual data frames) is therefore still safe for every root this
/// function is ever called on: exclusively-owned process roots built
/// the ordinary way, via ELF loading and demand paging, both of which
/// go through `map_page`. Top-level index 0 is the kernel's own shared
/// mapping (installed by `memory::create_process_page_table` as a copy
/// of the live kernel root -- see `memory::USER_SPACE_BASE`'s doc
/// comment) and must never be walked into here; every other top-level
/// index is guaranteed, by that same design, to be either empty or
/// entirely private to this one process.
///
/// # Safety
/// `root` must be a page table nothing is actively translating
/// through anymore -- not the live CR3/TTBR0_EL1 of this or any other
/// core, and not `memory_root` of any other still-alive task (a
/// `SharedThread` or an `IsolatedProcess` that fell back to sharing
/// its parent's table both alias another owner's root; freeing those
/// is exactly what `Task::owns_memory_root` exists to prevent -- see
/// its use in `task::run_schedule`). Calling this on a root that's
/// still referenced anywhere frees memory that's still live, which
/// the allocator can then hand out to something else while the old
/// mapping can still reach it.
pub unsafe fn free_process_page_table(root: *mut arch::PageTable) {
    use crate::memory::{phys_to_virt, vmm_free_frame};

    unsafe {
        // `root` (and every `..._phys` below) stays exactly the raw
        // physical address the frame allocator's bitmap is indexed by
        // -- that's what gets freed. The `..._table` pointers are a
        // separate, dereferenceable (translated) alias of the same
        // memory, used only for walking. Conflating the two would
        // either free the wrong bitmap index or, on x86_64, walk
        // through an unmapped physical-looking pointer -- see
        // `memory::phys_to_virt`'s doc comment.
        let root_virt = phys_to_virt(root as usize) as *mut arch::PageTable;

        for l0 in 1..512 {
            let e0 = &(*root_virt).entries[l0];
            if !e0.is_present() { continue; }
            let l1_phys = e0.physical_address();
            let l1_table = phys_to_virt(l1_phys) as *mut arch::PageTable;

            for l1 in 0..512 {
                let e1 = &(*l1_table).entries[l1];
                if !e1.is_present() { continue; }
                let l2_phys = e1.physical_address();
                let l2_table = phys_to_virt(l2_phys) as *mut arch::PageTable;

                for l2 in 0..512 {
                    let e2 = &(*l2_table).entries[l2];
                    if !e2.is_present() { continue; }
                    let l3_phys = e2.physical_address();
                    let l3_table = phys_to_virt(l3_phys) as *mut arch::PageTable;

                    for l3 in 0..512 {
                        let e3 = &(*l3_table).entries[l3];
                        if !e3.is_present() { continue; }
                        // Leaf: an actual page the process owned.
                        vmm_free_frame(e3.physical_address());
                    }
                    vmm_free_frame(l3_phys);
                }
                vmm_free_frame(l2_phys);
            }
            vmm_free_frame(l1_phys);
        }
        vmm_free_frame(root as usize);
    }
}

// =========================================================================
// Syscall-layer user-pointer validation
// =========================================================================

/// Returns the next-level table a present entry points to, or `None`
/// for a not-present one -- a read-only counterpart to `next_table`
/// (inside each `arch` module), since validation must never allocate
/// or mutate anything.
fn present_child(entry: &arch::PageTableEntry) -> Option<*mut arch::PageTable> {
    if entry.is_present() {
        Some(crate::memory::phys_to_virt(entry.physical_address()) as *mut arch::PageTable)
    } else {
        None
    }
}

/// Checks one 4KB page. `virt` must already be page-aligned. `root` is
/// a raw physical page-table-root address, same contract as every
/// `arch::*` entry point -- translated once here.
fn page_is_user_accessible(root: *mut arch::PageTable, virt: usize, need_write: bool) -> bool {
    let l0_idx = (virt >> 39) & 0x1FF;
    let l1_idx = (virt >> 30) & 0x1FF;
    let l2_idx = (virt >> 21) & 0x1FF;
    let l3_idx = (virt >> 12) & 0x1FF;

    unsafe {
        let root = crate::memory::phys_to_virt(root as usize) as *mut arch::PageTable;
        let Some(l1) = present_child(&(*root).entries[l0_idx]) else { return false };
        let Some(l2) = present_child(&(*l1).entries[l1_idx]) else { return false };
        let Some(l3) = present_child(&(*l2).entries[l2_idx]) else { return false };
        let leaf = &(*l3).entries[l3_idx];
        leaf.is_present() && leaf.is_user_accessible() && (!need_write || leaf.is_writable())
    }
}

/// Confirms `[ptr, ptr+len)` lies entirely within `root`'s own
/// present, user-accessible mapped memory (and is writable throughout,
/// if `need_write`) before the kernel dereferences a syscall's raw
/// caller-supplied pointer.
///
/// This exists because `sys_write`/`sys_read`/`sys_uname` used to
/// trust that pointer completely: `core::slice::from_raw_parts(ptr,
/// len)` on whatever a ring-3 process handed over, no check that it's
/// even inside that process's own address space. Since syscalls run
/// at ring 0/EL1 with the full kernel mapping active, a `ptr` pointing
/// at kernel memory would have been read or overwritten exactly like
/// any other buffer -- an arbitrary kernel-memory read/write primitive
/// available to any userspace program.
///
/// Only meaningful for an actual ring-3 caller; see
/// `syscall::validate_user_ptr`, the sole caller, for why a
/// SharedThread (kernel-mode) caller skips this entirely instead of
/// calling in with `root` = the kernel's own live table.
pub fn validate_user_range(root: usize, ptr: usize, len: usize, need_write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = ptr.checked_add(len) else { return false };
    if ptr < crate::memory::USER_SPACE_BASE {
        return false;
    }

    let root = root as *mut arch::PageTable;
    let first_page = ptr & !0xFFF;
    let last_page = (end - 1) & !0xFFF;

    let mut page = first_page;
    loop {
        if !page_is_user_accessible(root, page, need_write) {
            return false;
        }
        if page == last_page {
            return true;
        }
        page += 0x1000;
    }
}
