// src/hal/mod.rs

pub mod acpi;

pub fn init() {
    let mut uart = crate::uart::Uart::shared();

    let _ = core::fmt::Write::write_str(
        &mut uart,
        "mitosOS: Initializing Hardware Abstraction Layer...\n",
    );

    match acpi::get_limine_rsdp() {
        Ok(rsdp_addr) => {
            let _ = core::fmt::Write::write_fmt(
                &mut uart,
                format_args!(
                    "mitosOS: Limine ACPI RSDP located at virtual 0x{:X}\n",
                    rsdp_addr
                ),
            );

            match acpi::parse_rsdp(rsdp_addr) {
                Ok(root_table) => {
                    match root_table {
                        acpi::RootTable::Xsdt(addr) => {
                            let _ = core::fmt::Write::write_fmt(
                                &mut uart,
                                format_args!(
                                    "mitosOS: ACPI 2.0+ XSDT at physical 0x{:X}\n",
                                    addr
                                ),
                            );
                        }

                        acpi::RootTable::Rsdt(addr) => {
                            let _ = core::fmt::Write::write_fmt(
                                &mut uart,
                                format_args!(
                                    "mitosOS: ACPI 1.0 RSDT at physical 0x{:X}\n",
                                    addr
                                ),
                            );
                        }
                    }
                }

                Err(e) => {
                    let _ = core::fmt::Write::write_fmt(
                        &mut uart,
                        format_args!(
                            "mitosOS: ERR: Failed to parse RSDP - {}\n",
                            e
                        ),
                    );
                }
            }
        }

        Err(e) => {
            let _ = core::fmt::Write::write_fmt(
                &mut uart,
                format_args!(
                    "mitosOS: ERR: ACPI RSDP not found via Limine - {}\n",
                    e
                ),
            );
        }
    }
}
