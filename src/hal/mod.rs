// src/hal/mod.rs

pub mod acpi;

/// This used to also fetch the RSDP and parse it itself, in a second
/// copy of the exact same logic acpi::init() has -- both got called
/// from main.rs, so every RSDP failure printed twice (once from each
/// copy, under different message prefixes), and a fix applied to one
/// copy (as happened here) silently didn't apply to the other. Now
/// this just delegates; acpi::init() owns RSDP fetching, translation,
/// and parsing in exactly one place.
pub fn init() {
    let mut uart = crate::uart::Uart::shared();

    let _ = core::fmt::Write::write_str(
        &mut uart,
        "mitosOS: Initializing Hardware Abstraction Layer...\n",
    );

    acpi::init();
}
