//! ACPI table generation for fstart firmware.
//!
//! Builds on the [`acpi_tables`] crate from rust-vmm, adding:
//!
//! - **Static tables**: GTDT (Generic Timer), SPCR (Serial Port Console),
//!   DBG2 (Debug Port), IORT (IO Remapping), HPET (High Precision Timer).
//! - **AML extensions** ([`ext`]): `Sleep`, `Stall`, `ThermalZone`,
//!   `CondRefOf`, `RefOf`, `Increment`, `Decrement`.
//! - **Resource descriptors** ([`descriptors`]): GPIO Connection
//!   (GpioIo, GpioInt), I2C and SPI Serial Bus Connection.
//! - **Platform assemblers** ([`platform`]): architecture-specific ACPI
//!   table sets with a generic RSDP/XSDT/DSDT/FADT assembler.
//! - **Fixed-buffer sink** ([`sink::FixedBufSink`]): `AmlSink` backed
//!   by `&mut [u8]` for no-alloc output path.
//!
//! ## Architecture support
//!
//! - [`platform::arm`] -- MADT (GICv3), GTDT, ARM FADT flags.
//!   Gated behind the `arm` feature.
//! - [`platform::x86`] -- MADT (Local APIC + I/O APIC), HPET, x86 FADT.
//!   Gated behind the `x86` feature.
//!
//! ## Per-device ACPI
//!
//! Drivers implement [`device::AcpiDevice`] behind an `acpi` feature
//! gate to contribute DSDT entries and standalone tables.  ACPI-only
//! devices (no runtime driver) use the builders in [`devices`].

#![no_std]

extern crate alloc;

// Self-alias so that `fstart_acpi::` paths emitted by the acpi_dsl!
// proc-macro resolve correctly when the macro is used inside this crate.
extern crate self as fstart_acpi;

use alloc::vec;
use alloc::vec::Vec;

pub mod dbg2;
pub mod descriptors;
pub mod device;
pub mod devices;
pub mod ext;
pub mod gtdt;
pub mod iort;
pub mod platform;
pub mod sbsa;
pub mod sink;
pub mod spcr;
pub mod tock_bridge;

// Re-export commonly used types from acpi_tables.
pub use acpi_tables::aml;
pub use acpi_tables::fadt;
pub use acpi_tables::madt;
pub use acpi_tables::mcfg;
pub use acpi_tables::rsdp;
pub use acpi_tables::sdt;
pub use acpi_tables::xsdt;
pub use acpi_tables::{Aml, AmlSink};

/// AML NullTarget -- used as the target for binary operations whose
/// result is not stored (only returned as the expression value).
///
/// Emits a single `0x00` byte (NullName), which the AML interpreter
/// treats as "discard the store".
pub struct NullTarget;

impl Aml for NullTarget {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(0x00);
    }
}

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
    // SAFETY: T is repr(C, packed) with integer-only fields per doc contract.
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(val as *const T as *const u8, core::mem::size_of::<T>())
    };
    for (i, &b) in bytes.iter().enumerate() {
        sdt.write_u8(offset + i, b);
    }
}

/// Encode an AML PkgLength.
///
/// AML PkgLength encoding (ACPI spec 20.2.4):
/// - Total 0..63: 1 byte (6-bit length field)
/// - Total 64..4095: 2 bytes (byte count bits in byte 0 bits 6-7)
/// - Total 4096..1048575: 3 bytes
/// - Total 1048576+: 4 bytes
///
/// `content_len` is the size of the content *after* the PkgLength field.
/// The encoding includes the PkgLength field's own size in the total.
pub(crate) fn encode_pkg_length(content_len: usize) -> Vec<u8> {
    // Total includes the PkgLength field itself.
    let total1 = content_len + 1;
    if total1 < 0x40 {
        return vec![total1 as u8];
    }

    let total2 = content_len + 2;
    if total2 < 0x1000 {
        let byte0 = 0x40 | (total2 & 0x0F) as u8;
        let byte1 = (total2 >> 4) as u8;
        return vec![byte0, byte1];
    }

    let total3 = content_len + 3;
    if total3 < 0x10_0000 {
        let byte0 = 0x80 | (total3 & 0x0F) as u8;
        let byte1 = (total3 >> 4) as u8;
        let byte2 = (total3 >> 12) as u8;
        return vec![byte0, byte1, byte2];
    }

    let total4 = content_len + 4;
    let byte0 = 0xC0 | (total4 & 0x0F) as u8;
    let byte1 = (total4 >> 4) as u8;
    let byte2 = (total4 >> 12) as u8;
    let byte3 = (total4 >> 20) as u8;
    vec![byte0, byte1, byte2, byte3]
}

/// Serialize an Aml object to a `Vec<u8>`.
pub(crate) fn serialize(aml: &dyn Aml) -> Vec<u8> {
    let mut bytes = Vec::new();
    aml.to_aml_bytes(&mut bytes);
    bytes
}

/// Copy `src` into `dst` at the given offset.
pub(crate) fn copy_at(dst: &mut [u8], offset: usize, src: &[u8]) {
    dst[offset..offset + src.len()].copy_from_slice(src);
}
