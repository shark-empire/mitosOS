//! Professional Production-Ready ELF64 Executable Loader for mitosOS.

const EI_MAG0: usize = 0;
const EI_MAG1: usize = 1;
const EI_MAG2: usize = 2;
const EI_MAG3: usize = 3;
const ELFMAG0: u8 = 0x7F;
const ELFMAG1: u8 = b'E';
const ELFMAG2: u8 = b'L';
const ELFMAG3: u8 = b'F';

const EI_CLASS: usize = 4;
const ELFCLASS64: u8 = 2;

const EI_DATA: usize = 5;
const ELFDATA2LSB: u8 = 1;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;

#[cfg(target_arch = "x86_64")]
const EM_MACHINE: u16 = 62;

#[cfg(target_arch = "aarch64")]
const EM_MACHINE: u16 = 183;

const PT_LOAD: u32 = 1;

pub fn load_elf_to_process(elf_data: &[u8], page_table_root: usize) -> Result<usize, &'static str> {
    if elf_data.len() < 64 {
        return Err("ELF binary is too small to contain a valid header");
    }

    if elf_data[EI_MAG0] != ELFMAG0
        || elf_data[EI_MAG1] != ELFMAG1
        || elf_data[EI_MAG2] != ELFMAG2
        || elf_data[EI_MAG3] != ELFMAG3
    {
        return Err("Invalid ELF magic header signature");
    }

    if elf_data[EI_CLASS] != ELFCLASS64 {
        return Err("Unsupported ELF class: expected 64-bit (ELFCLASS64)");
    }

    if elf_data[EI_DATA] != ELFDATA2LSB {
        return Err("Unsupported ELF byte order: expected little-endian");
    }

    let e_type = u16::from_le_bytes(
        elf_data[16..18].try_into().map_err(|_| "Failed to read ELF type")?,
    );
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err("Unsupported ELF file type: must be Executable or Shared Object (PIE)");
    }

    let e_machine = u16::from_le_bytes(
        elf_data[18..20].try_into().map_err(|_| "Failed to read ELF machine architecture")?,
    );
    if e_machine != EM_MACHINE {
        return Err("ELF architecture mismatch for target CPU");
    }

    let entry_point = u64::from_le_bytes(
        elf_data[24..32].try_into().map_err(|_| "Failed to read ELF entry point")?,
    ) as usize;

    let ph_off = u64::from_le_bytes(
        elf_data[32..40].try_into().map_err(|_| "Failed to read program header offset")?,
    ) as usize;

    let ph_entsize = u16::from_le_bytes(
        elf_data[54..56].try_into().map_err(|_| "Failed to read program header entry size")?,
    ) as usize;

    let ph_num = u16::from_le_bytes(
        elf_data[56..58].try_into().map_err(|_| "Failed to read program header count")?,
    ) as usize;

    if ph_off == 0 || ph_num == 0 {
        return Err("ELF file contains no program headers");
    }

    let expected_ph_table_size = ph_off + (ph_num * ph_entsize.max(56));
    if elf_data.len() < expected_ph_table_size {
        return Err("Truncated ELF file: program headers exceed binary size");
    }

    for i in 0..ph_num {
        let ph_start = ph_off + (i * ph_entsize.max(56));
        if ph_start + 56 > elf_data.len() {
            return Err("Program header entry out of bounds");
        }

        let ph = &elf_data[ph_start..ph_start + 56];

        let p_type = u32::from_le_bytes(ph[0..4].try_into().unwrap());
        let p_flags = u32::from_le_bytes(ph[4..8].try_into().unwrap());
        let p_offset = u64::from_le_bytes(ph[8..16].try_into().unwrap()) as usize;
        let p_vaddr = u64::from_le_bytes(ph[16..24].try_into().unwrap()) as usize;
        let p_filesz = u64::from_le_bytes(ph[32..40].try_into().unwrap()) as usize;
        let p_memsz = u64::from_le_bytes(ph[40..48].try_into().unwrap()) as usize;

        if p_type == PT_LOAD {
            if p_filesz > p_memsz {
                return Err("Invalid segment size: file size exceeds memory size");
            }
            if p_offset + p_filesz > elf_data.len() {
                return Err("Segment file offset out of bounds");
            }

            let segment_data = &elf_data[p_offset..p_offset + p_filesz];

            unsafe {
                map_and_copy_segment(
                    page_table_root,
                    p_vaddr,
                    segment_data,
                    p_memsz,
                    p_flags,
                )?;
            }
        }
    }

    Ok(entry_point)
}

#[inline]
unsafe fn map_and_copy_segment(
    page_table_root: usize,
    vaddr: usize,
    data: &[u8],
    memsz: usize,
    _flags: u32,
) -> Result<(), &'static str> {
    unsafe {
        const PAGE_SIZE: usize = 4096;

        let start_vaddr = vaddr & !(PAGE_SIZE - 1);
        let end_vaddr = (vaddr + memsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let mut current_vaddr = start_vaddr;
        let mut data_offset = 0;

        while current_vaddr < end_vaddr {
            let phys_frame = crate::memory::alloc_frame()
                .ok_or("Out of memory: failed to allocate frame for ELF segment")?;

            crate::memory::map_page(page_table_root, current_vaddr, phys_frame)?;

            let page_offset = if current_vaddr < vaddr { vaddr - current_vaddr } else { 0 };
            let copy_start_in_page = page_offset;
            let bytes_left_in_data = data.len().saturating_sub(data_offset);
            let bytes_to_copy = core::cmp::min(bytes_left_in_data, PAGE_SIZE - copy_start_in_page);

            if bytes_to_copy > 0 {
                let dest_ptr = (current_vaddr + copy_start_in_page) as *mut u8;
                core::ptr::copy_nonoverlapping(
                    data[data_offset..data_offset + bytes_to_copy].as_ptr(),
                    dest_ptr,
                    bytes_to_copy,
                );
                data_offset += bytes_to_copy;
            }

            current_vaddr += PAGE_SIZE;
        }

        Ok(())
    }
}
