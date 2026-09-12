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
//! - **AML linker** ([`aml_linker::AmlWriter`]): fragment composition and
//!   SDT finalization directly into caller-provided storage.
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
#[cfg(test)]
extern crate std;

// Self-alias so that `fstart_acpi::` paths emitted by the acpi_dsl!
// proc-macro resolve correctly when the macro is used inside this crate.
extern crate self as fstart_acpi;

use alloc::vec;
use alloc::vec::Vec;

pub mod aml_linker;
pub mod dbg2;
pub mod descriptors;
pub mod device;
pub mod devices;
pub mod ext;
pub mod gtdt;
pub mod iort;
pub mod platform;
pub mod sbsa;
pub mod smbios;
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

/// Failure while binding or emitting AML. Failed fragments append no bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmlError {
    Capacity,
    LengthOverflow,
    InvalidOperand,
    InvalidRange,
    InvalidPath,
}

/// Convert a runtime scalar without discarding sign or high bits.
#[doc(hidden)]
pub fn checked_operand(value: impl TryInto<u64>) -> Result<u64, AmlError> {
    value.try_into().map_err(|_| AmlError::InvalidOperand)
}

/// An AML output sink that exposes the newly appended range for fixups.
///
/// This lets [`AmlFragment::emit`] perform one bulk copy followed by direct
/// fixed-width stores in the destination, without a fragment-sized stack copy.
pub trait AmlFragmentSink {
    /// Record a failed fragment so a table writer cannot finalize partial output.
    fn reject(&mut self, error: AmlError) -> Result<(), AmlError> {
        Err(error)
    }

    /// Append the entire fragment or leave the sink unchanged on error.
    /// On success the returned slice must be exactly the appended range.
    fn append_fragment<'a>(&'a mut self, bytes: &[u8]) -> Result<&'a mut [u8], AmlError>;
}

impl AmlFragmentSink for Vec<u8> {
    fn append_fragment<'a>(&'a mut self, bytes: &[u8]) -> Result<&'a mut [u8], AmlError> {
        let start = self.len();
        start
            .checked_add(bytes.len())
            .ok_or(AmlError::LengthOverflow)?;
        self.try_reserve(bytes.len())
            .map_err(|_| AmlError::Capacity)?;
        self.extend_from_slice(bytes);
        Ok(&mut self[start..])
    }
}

/// Encode a seven-character ACPI EISA identifier.
#[must_use]
pub const fn eisa_id(name: &str) -> u32 {
    let data = name.as_bytes();
    assert!(data.len() == 7, "EISA ID must contain seven characters");
    const fn hex(byte: u8) -> u32 {
        if byte <= b'9' {
            (byte - b'0') as u32
        } else {
            (byte - b'A' + 10) as u32
        }
    }
    (((data[0] - b'@') as u32) << 26
        | ((data[1] - b'@') as u32) << 21
        | ((data[2] - b'@') as u32) << 16
        | hex(data[3]) << 12
        | hex(data[4]) << 8
        | hex(data[5]) << 4
        | hex(data[6]))
    .swap_bytes()
}

/// Width of a runtime operand embedded in an [`AmlFragment`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixupKind {
    Byte,
    Word,
    DWord,
    QWord,
}

/// A bound used to derive a resource range length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixupValue {
    Literal(u64),
    Operand(usize),
}

/// Additional work associated with a runtime operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixupAction {
    None,
    /// Store `max - min + 1` at the descriptor's length field.
    RangeLength {
        offset: usize,
        min: FixupValue,
        max: FixupValue,
    },
}

/// Macro-generated location, encoding, and optional derived resource write.
/// Fields are private; the fragment constructor validates the metadata in const
/// evaluation. Low-level constructor misuse is a programmer error (panic).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fixup {
    offset: usize,
    kind: FixupKind,
    action: FixupAction,
    integer: bool,
}

impl Fixup {
    pub const fn new(offset: usize, kind: FixupKind) -> Self {
        Self {
            offset,
            kind,
            action: FixupAction::None,
            integer: true,
        }
    }

    /// Raw little-endian field, without an AML integer opcode.
    #[doc(hidden)]
    pub const fn raw(offset: usize, kind: FixupKind) -> Self {
        Self {
            integer: false,
            ..Self::new(offset, kind)
        }
    }

    pub const fn offset(self) -> usize {
        self.offset
    }
    pub const fn kind(self) -> FixupKind {
        self.kind
    }

    pub const fn with_range_length(
        offset: usize,
        kind: FixupKind,
        length_offset: usize,
        min: FixupValue,
        max: FixupValue,
    ) -> Self {
        Self {
            offset,
            kind,
            integer: false,
            action: FixupAction::RangeLength {
                offset: length_offset,
                min,
                max,
            },
        }
    }
}

/// AML compiled by `acpi_dsl!`.
///
/// Const operands are checked during compilation even in a local binding:
///
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! { Name("TEST", #{const byte 256u16}); };
/// ```
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! { Name("TEST", #{const qword -1i64}); };
/// ```
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! { Name("TEST", #{const qword 0x1_0000_0000_0000_0000u128}); };
/// ```
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! { Name("TEST", #{const qword 1.5f64}); };
/// ```
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! { Name("TEST", #{qword 1.5f64}); };
/// ```
/// ```compile_fail
/// use fstart_acpi_macros::acpi_dsl;
/// let _ = acpi_dsl! {
///     Name("RSC0", ResourceTemplate { IO(0x10000u32, 0x10000u32, 1u8, 8u8); });
/// };
/// ```
///
/// `bytes` contains the complete AML with zero-filled operand slots. Emission
/// is one bulk copy followed by fixed-width little-endian fixup stores (plus
/// derived resource-length stores); package lengths and literal encodings are
/// already final.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmlFragment<const N: usize, const K: usize> {
    bytes: [u8; N],
    fixups: [Fixup; K],
}

impl<const N: usize, const K: usize> AmlFragment<N, K> {
    /// Low-level macro constructor. Panics for malformed relocation metadata;
    /// macro-generated static fragments check this during compilation.
    #[doc(hidden)]
    pub const fn new(bytes: [u8; N], fixups: [Fixup; K]) -> Self {
        let mut index = 0;
        while index < K {
            let fixup = fixups[index];
            let width = fixup_width(fixup.kind);
            if fixup.integer {
                assert!(
                    fixup.offset > 0 && fixup.offset <= N,
                    "invalid AML integer patch"
                );
                assert!(
                    bytes[fixup.offset - 1] == fixup_prefix(fixup.kind),
                    "wrong AML integer encoding"
                );
            }
            assert!(
                fixup.offset <= N && width <= N - fixup.offset,
                "invalid AML fixup"
            );
            if let FixupAction::RangeLength { offset, min, max } = fixup.action {
                assert!(
                    offset <= N && width <= N - offset,
                    "invalid AML range fixup"
                );
                if let FixupValue::Operand(i) = min {
                    assert!(i < K, "invalid AML operand index");
                }
                if let FixupValue::Operand(i) = max {
                    assert!(i < K, "invalid AML operand index");
                }
            }
            index += 1;
        }
        Self { bytes, fixups }
    }

    pub const fn as_bytes(&self) -> &[u8; N] {
        &self.bytes
    }

    pub const fn fixups(&self) -> &[Fixup; K] {
        &self.fixups
    }

    #[doc(hidden)]
    pub const fn with_const_integer(self, offset: usize, kind: FixupKind, value: u64) -> Self {
        assert!(offset > 0 && offset <= N, "invalid const AML integer patch");
        assert!(
            self.bytes[offset - 1] == fixup_prefix(kind),
            "wrong const AML integer encoding"
        );
        self.with_const(offset, kind, value)
    }

    /// Apply a const-evaluable, explicitly-sized raw field during initialization.
    #[must_use]
    pub const fn with_const(mut self, offset: usize, kind: FixupKind, value: u64) -> Self {
        let width = fixup_width(kind);
        assert!(
            offset <= N && width <= N - offset,
            "invalid const AML patch"
        );
        assert!(
            value_fits(value, width),
            "const AML operand exceeds its declared width"
        );
        let bytes = value.to_le_bytes();
        let mut index = 0;
        while index < width {
            self.bytes[offset + index] = bytes[index];
            index += 1;
        }
        self
    }

    pub fn emit(
        &self,
        out: &mut impl AmlFragmentSink,
        operands: &[u64; K],
    ) -> Result<(), AmlError> {
        // Preflight *all* values, including derived fields, before appending.
        for (fixup, value) in self.fixups.iter().zip(operands) {
            if !value_fits(*value, fixup_width(fixup.kind)) {
                return out.reject(AmlError::InvalidOperand);
            }
            if let Err(error) = self.range_length(fixup, operands) {
                return out.reject(error);
            }
        }
        let bytes = out.append_fragment(&self.bytes)?;
        for (fixup, value) in self.fixups.iter().zip(operands) {
            let width = fixup_width(fixup.kind);
            bytes[fixup.offset..fixup.offset + width]
                .copy_from_slice(&value.to_le_bytes()[..width]);
            if let Some((offset, length)) = self.range_length(fixup, operands)? {
                bytes[offset..offset + width].copy_from_slice(&length.to_le_bytes()[..width]);
            }
        }
        Ok(())
    }

    fn range_length(
        &self,
        fixup: &Fixup,
        operands: &[u64; K],
    ) -> Result<Option<(usize, u64)>, AmlError> {
        let FixupAction::RangeLength { offset, min, max } = fixup.action else {
            return Ok(None);
        };
        let length = fixup_value(max, operands)
            .checked_sub(fixup_value(min, operands))
            .and_then(|value| value.checked_add(1))
            .filter(|value| value_fits(*value, fixup_width(fixup.kind)))
            .ok_or(AmlError::InvalidRange)?;
        Ok(Some((offset, length)))
    }
}

/// An AML fragment paired with the runtime expressions written into its fixups.
///
/// `acpi_dsl!` produces this type when the DSL contains typed runtime operands,
/// preserving the source-order relationship between each expression and fixup.
/// Invalid scalar conversions are retained here and returned by `emit` before
/// any output is appended. `From<BoundAmlFragment> for Vec<u8>` is a panicking
/// convenience; callers that handle construction failure should use `emit`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundAmlFragment<const N: usize, const K: usize> {
    fragment: &'static AmlFragment<N, K>,
    operands: [u64; K],
    error: Option<AmlError>,
}

impl<const N: usize> core::ops::Deref for AmlFragment<N, 0> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl<const N: usize> From<AmlFragment<N, 0>> for Vec<u8> {
    fn from(fragment: AmlFragment<N, 0>) -> Self {
        fragment.bytes.to_vec()
    }
}

impl<const N: usize, const K: usize> BoundAmlFragment<N, K> {
    pub const fn new(
        fragment: &'static AmlFragment<N, K>,
        operands: [Result<u64, AmlError>; K],
    ) -> Self {
        let mut values = [0; K];
        let mut error = None;
        let mut index = 0;
        while index < K {
            match operands[index] {
                Ok(value) => values[index] = value,
                Err(value) if error.is_none() => error = Some(value),
                Err(_) => {}
            }
            index += 1;
        }
        Self {
            fragment,
            operands: values,
            error,
        }
    }

    /// Copy the precompiled bytes and apply the bound operands.
    pub fn emit(&self, sink: &mut impl AmlFragmentSink) -> Result<(), AmlError> {
        if let Some(error) = self.error {
            return sink.reject(error);
        }
        self.fragment.emit(sink, &self.operands)
    }

    pub const fn fragment(&self) -> &'static AmlFragment<N, K> {
        self.fragment
    }

    pub const fn operands(&self) -> Result<&[u64; K], AmlError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(&self.operands),
        }
    }
}

impl<const N: usize, const K: usize> From<BoundAmlFragment<N, K>> for Vec<u8> {
    fn from(fragment: BoundAmlFragment<N, K>) -> Self {
        let mut bytes = Vec::new();
        fragment
            .emit(&mut bytes)
            .expect("invalid AML fragment binding");
        bytes
    }
}

const fn fixup_prefix(kind: FixupKind) -> u8 {
    match kind {
        FixupKind::Byte => 0x0a,
        FixupKind::Word => 0x0b,
        FixupKind::DWord => 0x0c,
        FixupKind::QWord => 0x0e,
    }
}

const fn fixup_width(kind: FixupKind) -> usize {
    match kind {
        FixupKind::Byte => 1,
        FixupKind::Word => 2,
        FixupKind::DWord => 4,
        FixupKind::QWord => 8,
    }
}

const fn value_fits(value: u64, width: usize) -> bool {
    width == 8 || value < (1u64 << (width * 8))
}

fn fixup_value<const K: usize>(value: FixupValue, operands: &[u64; K]) -> u64 {
    match value {
        FixupValue::Literal(value) => value,
        FixupValue::Operand(index) => operands[index],
    }
}

impl<const N: usize> Aml for AmlFragment<N, 0> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.vec(&self.bytes);
    }
}

/// Already-encoded AML that can be embedded in another AML object.
pub struct RawAml<'a>(pub &'a [u8]);

impl Aml for RawAml<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.vec(self.0);
    }
}

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

/// Small legacy ISA IRQ resource descriptor.
///
/// ACPI has two IRQ resource encodings. `acpi_tables::aml::Interrupt` emits
/// the large Extended Interrupt descriptor (`0x89`), which is appropriate for
/// GSIs and non-ISA interrupt models. PC/AT legacy devices such as i8042,
/// PIT, PIC, and x87 use the small `IRQ (...) { n }` descriptor (`0x23`) like
/// coreboot's ASL. Linux treats these as ISA IRQ resources.
#[derive(Debug, Clone, Copy)]
pub struct IsaIrq {
    edge: bool,
    active_high: bool,
    exclusive: bool,
    irq_mask: u16,
    with_flags: bool,
}

impl IsaIrq {
    /// Create a small IRQ descriptor for one ISA IRQ line.
    pub const fn new(edge: bool, active_high: bool, exclusive: bool, irq: u8) -> Self {
        Self {
            edge,
            active_high,
            exclusive,
            irq_mask: 1u16 << irq,
            with_flags: true,
        }
    }

    /// Create a small IRQNoFlags descriptor for one ISA IRQ line.
    pub const fn no_flags(irq: u8) -> Self {
        Self {
            edge: true,
            active_high: true,
            exclusive: true,
            irq_mask: 1u16 << irq,
            with_flags: false,
        }
    }

    /// Create a small IRQ descriptor from an IRQ bitmask.
    pub const fn from_mask(edge: bool, active_high: bool, exclusive: bool, irq_mask: u16) -> Self {
        Self {
            edge,
            active_high,
            exclusive,
            irq_mask,
            with_flags: true,
        }
    }
}

impl Aml for IsaIrq {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        if self.with_flags {
            // Small item: type=0, name=4 (IRQ), length=3 => 0x23.
            sink.byte(0x23);
            sink.word(self.irq_mask);
            let flags = (self.edge as u8)
                | ((!self.active_high as u8) << 3)
                | ((!self.exclusive as u8) << 4);
            sink.byte(flags);
        } else {
            // Small item IRQNoFlags, length=2 => 0x22. ACPI defaults this to
            // edge-triggered, active-high, exclusive for ISA IRQs.
            sink.byte(0x22);
            sink.word(self.irq_mask);
        }
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
