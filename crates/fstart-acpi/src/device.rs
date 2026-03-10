//! Per-device ACPI trait.
//!
//! Drivers implement [`AcpiDevice`] behind a feature gate to contribute
//! DSDT entries and standalone tables (SPCR, MCFG, etc.) to the ACPI
//! table set.  Each driver defines the ACPI fields it needs in its own
//! `Config` struct — there is no universal context type.

extern crate alloc;

use alloc::vec::Vec;

/// A device that can contribute to ACPI table generation.
///
/// Implemented by driver structs behind an `acpi` feature gate.
/// The driver uses its own hardware knowledge (`_HID`, register layout,
/// capabilities) combined with its `Config` fields (`acpi_name`,
/// `acpi_gsiv`, `acpi_irq`, etc.) to produce AML.
///
/// # Design
///
/// The associated `Config` type is the driver's existing config struct
/// (e.g., `Pl011Config`), extended with `#[serde(default)]` ACPI fields.
/// ACPI data that is board-specific (ACPI name, interrupt number) lives
/// in these config fields; data that is driver-intrinsic (`_HID`, MMIO
/// size, register semantics) is hardcoded in the implementation.
///
/// `dsdt_aml()` returns raw serialized AML bytes (`Vec<u8>`) rather than
/// `&dyn Aml` to avoid lifetime issues with temporary AML objects.
/// The caller places the returned bytes inside a `\_SB` scope in the DSDT.
pub trait AcpiDevice {
    /// The driver's configuration type (same as `Device::Config`).
    type Config;

    /// Produce AML bytes for this device's DSDT entry.
    ///
    /// Returns serialized AML for an ACPI `Device` node.  The bytes are
    /// appended inside the `\_SB` scope of the DSDT.  The implementation
    /// builds the complete device: `_HID`, `_UID`, `_CRS` (MMIO +
    /// interrupts), and any device-specific objects (`_CCA`, `_CLS`,
    /// `_OSC`, `_DSD`, power resources, etc.).
    fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8>;

    /// Produce standalone ACPI tables for this device as serialized bytes.
    ///
    /// Each `Vec<u8>` is a complete, self-contained ACPI table (with valid
    /// header and checksum).  UARTs return SPCR, PCIe root complexes
    /// return MCFG.  Most devices return an empty vec.
    fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
        let _ = config;
        Vec::new()
    }
}
