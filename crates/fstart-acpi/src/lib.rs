//! ACPI table generation for fstart firmware.
//!
//! Builds on the [`acpi_tables`] crate from rust-vmm, adding tables
//! the upstream crate does not yet provide: GTDT (Generic Timer) and
//! SPCR (PL011 Serial Port Console).
//!
//! The [`platform`] module contains the generic ACPI assembler (RSDP,
//! XSDT, DSDT, FADT) and architecture-specific sub-modules:
//!
//! - [`platform::arm`] — MADT (GICv3), GTDT, ARM FADT flags.
//!   Gated behind the `arm` feature.
//!
//! The [`sbsa`] module is a preserved standalone builder for QEMU
//! SBSA-ref platforms (used by older tests).

#![no_std]

extern crate alloc;

pub mod dbg2;
pub mod device;
pub mod devices;
pub mod gtdt;
pub mod iort;
pub mod platform;
pub mod sbsa;
pub mod spcr;

// Re-export commonly used types from acpi_tables.
pub use acpi_tables::aml;
pub use acpi_tables::fadt;
pub use acpi_tables::madt;
pub use acpi_tables::mcfg;
pub use acpi_tables::rsdp;
pub use acpi_tables::sdt;
pub use acpi_tables::xsdt;
pub use acpi_tables::{Aml, AmlSink};

/// OEM ID used in all fstart-generated ACPI tables (6 bytes, padded).
pub const OEM_ID: [u8; 6] = *b"FSTART";

/// OEM Table ID used in all fstart-generated ACPI tables (8 bytes).
pub const OEM_TABLE_ID: [u8; 8] = *b"FSTARTFW";

/// OEM revision for fstart ACPI tables.
pub const OEM_REVISION: u32 = 1;

/// Align a value up to the given power-of-two alignment.
#[inline]
pub const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}
