// src/hal/mod.rs

pub mod acpi;

pub fn init() {
    let mut uart = crate::uart::Uart::shared();
    let _ = core::fmt::Write::write_str(&mut uart, "mitosOS: Initializing Hardware Abstraction Layer...\n");

    match acpi::find_rsdp_legacy() {
        Some(rsdp_addr) => {
            let _ = core::fmt::Write::write_fmt(
                &mut uart,
                format_args!("mitosOS: ACPI RSDP located at 0x{:X}\n", rsdp_addr)
            );
            
            match acpi::parse_rsdp(rsdp_addr) {
                Ok(root_table_addr) => {
                    let _ = core::fmt::Write::write_fmt(
                        &mut uart,
                        format_args!("mitosOS: ACPI Root Table (RSDT/XSDT) at 0x{:X}\n", root_table_addr)
                    );
                    // Next step will be parsing this root table for the MADT and MCFG
                }
                Err(e) => {
                    let _ = core::fmt::Write::write_fmt(
                        &mut uart,
                        format_args!("mitosOS: ERR: Failed to parse RSDP - {}\n", e)
                    );
                }
            }
        },
        None => {
            let _ = core::fmt::Write::write_str(&mut uart, "mitosOS: ERR: ACPI RSDP not found in legacy BIOS memory!\n");
        }
    }
}
