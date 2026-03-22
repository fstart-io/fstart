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

/// Write a `#[repr(C, packed)]` struct into an [`Sdt`] at the given byte
/// offset.
///
/// This is the Rust equivalent of `*(struct foo *)(buf + off) = val;` in C.
/// All ACPI table fields are little-endian; since every fstart target (and
/// the host x86 test runner) is LE, native `repr(C)` layout produces the
/// correct wire bytes.
///
/// # Safety
///
/// `T` must be `#[repr(C, packed)]` with only integer fields (no padding,
/// no references, no `Drop`).  The caller must ensure `offset + size_of::<T>()`
/// does not exceed the SDT's allocated length.
pub fn write_struct<T: Copy>(sdt: &mut sdt::Sdt, offset: usize, val: &T) {
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(val as *const T as *const u8, core::mem::size_of::<T>())
    };
    for (i, &b) in bytes.iter().enumerate() {
        sdt.write_u8(offset + i, b);
    }
}
