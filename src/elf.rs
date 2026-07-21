//! Minimal ELF64 executable loader for mitosOS.

pub fn load_elf_to_process(elf_data: &[u8], _page_table_root: usize) -> Result<usize, &'static str> {
    // 1. Verify ELF Magic Number (\x7FELF)
    if elf_data.len() < 64 || &elf_data[0..4] != b"\x7FELF" {
        return Err("Invalid ELF magic header");
    }

    // 2. Read the Entry Point address (located at offset 24 in an ELF64 header)
    let entry_point = u64::from_le_bytes(elf_data[24..32].try_into().map_err(|_| "Failed to read ELF entry point")?) as usize;

    // TODO: Parse program headers here to copy segments into the isolated page table.
    Ok(entry_point)
}
