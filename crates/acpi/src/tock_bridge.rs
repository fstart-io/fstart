//! Bridge between tock-registers definitions and ACPI Field AML.
//!
//! Converts register bitfield metadata from [`tock_registers::RegisterDebugInfo`]
//! into ACPI [`FieldEntry`] values, then serializes them as an
//! `OperationRegion` + `Field` pair.
//!
//! This makes tock-registers the single source of truth for register
//! layouts: the same definitions drive both firmware MMIO access and
//! OS-visible ACPI register descriptions.
//!
//! # Usage
//!
//! ```ignore
//! use fstart_acpi::tock_bridge::TockAcpiField;
//! use fstart_acpi::aml::OpRegionSpace;
//!
//! // PL011 registers defined with register_bitfields! elsewhere
//! let field = TockAcpiField::<u32, pl011::FR::Register>::new(
//!     "FREG",                    // ACPI OperationRegion name
//!     OpRegionSpace::SystemMemory,
//!     0x18,                      // register offset from base
//! );
//!
//! // Use in acpi_dsl! via interpolation:
//! // #{field}  -- emits OpRegion + Field AML
//! ```

extern crate alloc;

use alloc::vec::Vec;

use acpi_tables::aml::{
    Field, FieldAccessType, FieldEntry, FieldLockRule, FieldUpdateRule, OpRegion, OpRegionSpace,
    Path,
};
use acpi_tables::{Aml, AmlSink};
use tock_registers::UIntLike;
use tock_registers::debug::RegisterDebugInfo;
use tock_registers::fields::Field as TockField;

/// Convert tock-registers [`RegisterDebugInfo`] metadata into ACPI
/// [`FieldEntry`] values.
///
/// Iterates fields sorted by bit offset, inserting `Reserved` gaps
/// for any unnamed bit ranges between fields.
///
/// Returns a `Vec<FieldEntry>` suitable for passing to [`Field::new`].
pub fn tock_field_entries<T, R>(total_bits: usize) -> Vec<FieldEntry>
where
    T: UIntLike + 'static,
    R: RegisterDebugInfo<T> + 'static,
{
    let fields: &[TockField<T, R>] = R::fields();
    let names: &[&str] = R::field_names();

    // Collect (shift, width, name) tuples and sort by bit position.
    // Width is computed by counting how many low bits are set in the mask.
    // tock-registers masks are contiguous 1-bits (bitmask!(n) = (1<<n)-1),
    // so we count trailing ones after shifting would give us the mask.
    // Simpler: iterate bits by shifting the mask right until it's zero.
    let mut entries: Vec<(usize, usize, &str)> = fields
        .iter()
        .zip(names.iter())
        .map(|(f, &name)| {
            let shift = f.shift;
            let width = mask_width(f.mask);
            (shift, width, name)
        })
        .collect();
    entries.sort_by_key(|&(shift, _, _)| shift);

    // Compress tock-registers names to 4-char ACPI NameSegs.
    // Short names (<= 4) are padded with '_'.
    // Long names (> 4) are truncated; collisions are resolved by
    // replacing the last character with a digit (0-9).
    let acpi_names = compress_names(entries.iter().map(|(_, _, n)| *n).collect());

    let mut result = Vec::new();
    let mut bit_pos: usize = 0;

    for ((shift, width, _), acpi_name) in entries.iter().zip(acpi_names.iter()) {
        // Insert gap if there's unnamed space before this field.
        if *shift > bit_pos {
            result.push(FieldEntry::Reserved(*shift - bit_pos));
        }

        result.push(FieldEntry::Named(*acpi_name, *width));
        bit_pos = *shift + *width;
    }

    // Trailing gap to fill out the register width.
    if bit_pos < total_bits {
        result.push(FieldEntry::Reserved(total_bits - bit_pos));
    }

    result
}

/// Compress a list of tock-registers field names to 4-char ACPI NameSegs.
///
/// - Names <= 4 chars: padded with `_` (e.g., `TXE` -> `TXE_`).
/// - Names > 4 chars: truncated to first 4 chars (e.g., `UARTEN` -> `UART`).
/// - Collisions from truncation: last char replaced with digit suffix
///   (e.g., two names starting with `UART` -> `UART`, `UAR0`; three ->
///   `UART`, `UAR0`, `UAR1`).
fn compress_names(names: Vec<&str>) -> Vec<[u8; 4]> {
    // First pass: truncate/pad all names to 4 bytes.
    let mut candidates: Vec<[u8; 4]> = names
        .iter()
        .map(|name| {
            let mut buf = [b'_'; 4];
            for (i, b) in name.bytes().take(4).enumerate() {
                buf[i] = b;
            }
            buf
        })
        .collect();

    // Second pass: detect and resolve collisions.
    // For each collision group, keep the first occurrence as-is and
    // disambiguate subsequent ones by replacing the last char with
    // 0-9, A-Z (36 possible suffixes -- more than enough).
    let len = candidates.len();
    for i in 0..len {
        let mut suffix = 0u8;
        for j in (i + 1)..len {
            if candidates[j] == candidates[i] {
                let mut fixed = candidates[i];
                fixed[3] = suffix_char(suffix);
                suffix += 1;
                candidates[j] = fixed;
            }
        }
    }

    candidates
}

/// Map a collision suffix index to a valid ACPI NameSeg character.
/// 0-9 -> '0'-'9', 10-35 -> 'A'-'Z'.
fn suffix_char(idx: u8) -> u8 {
    if idx < 10 {
        b'0' + idx
    } else {
        b'A' + (idx - 10)
    }
}

/// Count the number of set bits in a UIntLike mask.
///
/// tock-registers masks are contiguous low bits (`bitmask!(n)` = `(1<<n)-1`),
/// so we shift right until zero and count iterations.
fn mask_width<T: UIntLike>(mask: T) -> usize {
    let zero = T::zero();
    let mut val = mask;
    let mut count = 0usize;
    while val != zero {
        count += 1;
        val = val >> 1;
    }
    count
}

/// Access type selection based on the register width.
fn access_type_for_width(bits: usize) -> FieldAccessType {
    match bits {
        8 => FieldAccessType::Byte,
        16 => FieldAccessType::Word,
        32 => FieldAccessType::DWord,
        64 => FieldAccessType::QWord,
        _ => FieldAccessType::Any,
    }
}

/// A complete ACPI `OperationRegion` + `Field` pair derived from a
/// tock-registers register definition.
///
/// Implements [`Aml`] so it can be used directly in `acpi_dsl!` via
/// `#{expr}` interpolation or passed to any builder that takes `&dyn Aml`.
pub struct TockAcpiField {
    region_name: &'static str,
    space: OpRegionSpace,
    offset: u64,
    region_bytes: u64,
    access_type: FieldAccessType,
    field_entries: Vec<FieldEntry>,
}

impl TockAcpiField {
    /// Create from a tock-registers `RegisterDebugInfo` type.
    ///
    /// - `region_name`: 4-char ACPI name for the OperationRegion (e.g., "FREG")
    /// - `space`: address space (SystemMemory, PCIConfig, etc.)
    /// - `offset`: byte offset of this register from the region base
    pub fn new<T, R>(region_name: &'static str, space: OpRegionSpace, offset: u64) -> Self
    where
        T: UIntLike + 'static,
        R: RegisterDebugInfo<T> + 'static,
    {
        let reg_bits = core::mem::size_of::<T>() * 8;
        let reg_bytes = core::mem::size_of::<T>() as u64;
        let field_entries = tock_field_entries::<T, R>(reg_bits);
        Self {
            region_name,
            space,
            offset,
            region_bytes: reg_bytes,
            access_type: access_type_for_width(reg_bits),
            field_entries,
        }
    }

    /// Create from a pre-built list of field entries with explicit
    /// region size and access type.
    ///
    /// Useful when combining multiple registers into a single ACPI
    /// Field definition (e.g., a PCI config space region spanning
    /// multiple registers with `Offset()` gaps).
    pub fn from_entries(
        region_name: &'static str,
        space: OpRegionSpace,
        offset: u64,
        region_bytes: u64,
        access_type: FieldAccessType,
        field_entries: Vec<FieldEntry>,
    ) -> Self {
        Self {
            region_name,
            space,
            offset,
            region_bytes,
            access_type,
            field_entries,
        }
    }
}

impl Aml for TockAcpiField {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        // OperationRegion
        let offset_val = self.offset;
        let length_val = self.region_bytes;
        let opreg = OpRegion::new(
            Path::new(self.region_name),
            self.space,
            &offset_val,
            &length_val,
        );
        opreg.to_aml_bytes(sink);

        // Field
        let field = Field::new(
            Path::new(self.region_name),
            self.access_type,
            FieldLockRule::NoLock,
            FieldUpdateRule::Preserve,
            self.field_entries.clone(),
        );
        field.to_aml_bytes(sink);
    }
}

/// Build a `(byte_offset, bit_width, field_entries)` tuple for one
/// register, using `offset_of!` to derive the byte offset from a
/// `register_structs!` type.
pub fn reg_entry<T, R>(struct_offset: usize) -> (u64, usize, Vec<FieldEntry>)
where
    T: UIntLike + 'static,
    R: RegisterDebugInfo<T> + 'static,
{
    let bits = core::mem::size_of::<T>() * 8;
    (struct_offset as u64, bits, tock_field_entries::<T, R>(bits))
}

/// Trait for register field types that carry type-level register metadata.
///
/// Implemented for `tock_registers::registers::ReadWrite<T, R>` and
/// `ReadOnly<T, R>`.  Custom register wrappers (e.g., fstart-core::mmio's
/// `MmioReadWrite`) can implement this to participate in [`tock_acpi_field!`].
pub trait AcpiRegField {
    /// The register width type (u8, u16, u32, u64).
    type Width: UIntLike + 'static;
    /// The `RegisterDebugInfo` type from `register_bitfields!`.
    type Reg: RegisterDebugInfo<Self::Width> + 'static;
}

impl<T: UIntLike + 'static, R: RegisterDebugInfo<T> + 'static> AcpiRegField
    for tock_registers::registers::ReadWrite<T, R>
{
    type Width = T;
    type Reg = R;
}

impl<T: UIntLike + 'static, R: RegisterDebugInfo<T> + 'static> AcpiRegField
    for tock_registers::registers::ReadOnly<T, R>
{
    type Width = T;
    type Reg = R;
}

/// Extract register metadata from a closure that accesses a struct field.
///
/// The closure is never called -- it exists purely so that the compiler
/// can infer `T` and `R` from the struct field type
/// (`ReadWrite<T, R>` or `ReadOnly<T, R>`).
pub fn field_entry_for<S, F, Fld>(_accessor: F, offset: usize) -> (u64, usize, Vec<FieldEntry>)
where
    F: Fn(&S) -> &Fld,
    Fld: AcpiRegField,
{
    reg_entry::<Fld::Width, Fld::Reg>(offset)
}

/// Build a [`TockAcpiField`] from a `register_structs!` type.
///
/// Takes the struct type, region metadata, and a list of
/// `(field_name, register_width_type, RegisterType)` tuples.
/// Uses `core::mem::offset_of!` to derive byte offsets from the
/// struct layout automatically.
///
/// # Example
///
/// ```ignore
/// register_structs! {
///     MchRegs {
///         (0x000 => _pad0),
///         (0x040 => pub epbar: ReadWrite<u32, MCH_EPBAR::Register>),
///         (0x044 => pub mchbar: ReadWrite<u32, MCH_MCHBAR::Register>),
///         (0x048 => @END),
///     }
/// }
///
/// let mchp = tock_acpi_field!(MchRegs, "MCHP", PCIConfig, DWord, [
///     epbar, mchbar,
/// ]);
/// ```
#[macro_export]
macro_rules! tock_acpi_field {
    (
        $struct_ty:ty,
        $region_name:expr,
        $space:ident,
        $access:ident,
        [ $( $field:ident ),* $(,)? ]
    ) => {{
        extern crate alloc as __tock_acpi_alloc;
        let entries: __tock_acpi_alloc::vec::Vec<(u64, usize, __tock_acpi_alloc::vec::Vec<$crate::aml::FieldEntry>)> =
            {
                let mut v = __tock_acpi_alloc::vec::Vec::new();
                $(
                    v.push($crate::tock_bridge::field_entry_for(
                        |s: &$struct_ty| &s.$field,
                        ::core::mem::offset_of!($struct_ty, $field),
                    ));
                )*
                v
            };
        let refs: __tock_acpi_alloc::vec::Vec<(u64, usize, &[$crate::aml::FieldEntry])> =
            entries.iter().map(|(o, b, e)| (*o, *b, e.as_slice())).collect();
        $crate::tock_bridge::build_multi_register_field(
            $region_name,
            $crate::aml::OpRegionSpace::$space,
            0,
            ::core::mem::size_of::<$struct_ty>() as u64,
            $crate::aml::FieldAccessType::$access,
            &refs,
        )
    }};
}

/// Build a multi-register ACPI Field spanning a byte range.
///
/// Takes a list of `(byte_offset, register_bits, field_entries)` tuples
/// for each register in the region. Combines them into a single
/// `OperationRegion` + `Field` with `Reserved` gaps between registers.
///
/// This matches the coreboot pattern of a single `OperationRegion`
/// covering an entire PCI config space block (e.g., 0x00..0x100) with
/// a `Field` that uses `Offset()` to jump between register groups.
pub fn build_multi_register_field(
    region_name: &'static str,
    space: OpRegionSpace,
    base_offset: u64,
    total_bytes: u64,
    access_type: FieldAccessType,
    registers: &[(u64, usize, &[FieldEntry])],
) -> TockAcpiField {
    let mut entries = Vec::new();
    let mut bit_pos: usize = 0;

    for &(reg_offset, _reg_bits, reg_fields) in registers {
        let target_bits = ((reg_offset - base_offset) as usize) * 8;
        if target_bits > bit_pos {
            entries.push(FieldEntry::Reserved(target_bits - bit_pos));
            bit_pos = target_bits;
        }
        for entry in reg_fields {
            match entry {
                FieldEntry::Named(name, bits) => {
                    entries.push(FieldEntry::Named(*name, *bits));
                    bit_pos += bits;
                }
                FieldEntry::Reserved(bits) => {
                    entries.push(FieldEntry::Reserved(*bits));
                    bit_pos += bits;
                }
            }
        }
    }

    // Fill to total region size.
    let total_bits = (total_bytes as usize) * 8;
    if bit_pos < total_bits {
        entries.push(FieldEntry::Reserved(total_bits - bit_pos));
    }

    TockAcpiField::from_entries(
        region_name,
        space,
        base_offset,
        total_bytes,
        access_type,
        entries,
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use tock_registers::register_bitfields;

    // Define test registers using tock-registers macros.
    register_bitfields! [u32,
        /// Test Flag Register (like PL011 FR)
        TEST_FR [
            RXFE OFFSET(4) NUMBITS(1) [],
            TXFF OFFSET(5) NUMBITS(1) []
        ],
        /// Test Control Register (like PL011 CR)
        TEST_CR [
            UARTEN OFFSET(0) NUMBITS(1) [],
            TXE OFFSET(8) NUMBITS(1) [],
            RXE OFFSET(9) NUMBITS(1) []
        ]
    ];

    #[test]
    fn test_tock_field_entries_basic() {
        let entries = tock_field_entries::<u32, TEST_FR::Register>(32);

        // TEST_FR has RXFE at bit 4 (1 bit) and TXFF at bit 5 (1 bit).
        // Expected layout: Reserved(4), RXFE(1), TXFF(1), Reserved(26)
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0], FieldEntry::Reserved(4));
        assert_eq!(entries[1], FieldEntry::Named(*b"RXFE", 1));
        assert_eq!(entries[2], FieldEntry::Named(*b"TXFF", 1));
        assert_eq!(entries[3], FieldEntry::Reserved(26));
    }

    #[test]
    fn test_tock_field_entries_gaps() {
        let entries = tock_field_entries::<u32, TEST_CR::Register>(32);

        // TEST_CR: UARTEN at bit 0 (1 bit), TXE at bit 8 (1 bit), RXE at bit 9 (1 bit).
        // Expected: UARTEN(1), Reserved(7), TXE(1), RXE(1), Reserved(22)
        assert_eq!(entries.len(), 5);

        assert_eq!(entries[0], FieldEntry::Named(*b"UART", 1));
        assert_eq!(entries[1], FieldEntry::Reserved(7));
        assert_eq!(entries[2], FieldEntry::Named(*b"TXE_", 1));
        assert_eq!(entries[3], FieldEntry::Named(*b"RXE_", 1));
        assert_eq!(entries[4], FieldEntry::Reserved(22));
    }

    // ---------------------------------------------------------------
    // compress_names tests
    // ---------------------------------------------------------------

    #[test]
    fn test_compress_short_names() {
        // Names <= 4 chars: padded with '_'.
        let result = compress_names(vec!["TXE", "RXE", "DR"]);
        assert_eq!(result, vec![*b"TXE_", *b"RXE_", *b"DR__"]);
    }

    #[test]
    fn test_compress_exact_4_chars() {
        let result = compress_names(vec!["EPEN", "MHEN", "TLUD"]);
        assert_eq!(result, vec![*b"EPEN", *b"MHEN", *b"TLUD"]);
    }

    #[test]
    fn test_compress_long_names_no_collision() {
        // Names > 4 chars truncated to first 4.
        let result = compress_names(vec!["UARTEN", "FIFO_EN", "BYPASS"]);
        assert_eq!(result, vec![*b"UART", *b"FIFO", *b"BYPA"]);
    }

    #[test]
    fn test_compress_long_names_with_collision() {
        // Two names that truncate to the same 4 chars.
        // First keeps its truncated name, second gets last char replaced with '0'.
        let result = compress_names(vec!["UARTEN", "UARTDIS"]);
        assert_eq!(result[0], *b"UART");
        assert_eq!(result[1], *b"UAR0");
    }

    #[test]
    fn test_compress_triple_collision() {
        // Three names truncating to "FIFO".
        let result = compress_names(vec!["FIFO_EN", "FIFO_RST", "FIFO_CLR"]);
        assert_eq!(result[0], *b"FIFO");
        assert_eq!(result[1], *b"FIF0");
        assert_eq!(result[2], *b"FIF1");
    }

    #[test]
    fn test_compress_mixed() {
        // Mix of short, exact, long, and colliding names.
        let result = compress_names(vec!["TX", "RXFE", "UARTEN", "UARTCLK", "VCO_GAIN"]);
        assert_eq!(result[0], *b"TX__");
        assert_eq!(result[1], *b"RXFE");
        assert_eq!(result[2], *b"UART");
        assert_eq!(result[3], *b"UAR0"); // collision with UART
        assert_eq!(result[4], *b"VCO_");
    }

    #[test]
    fn test_compress_many_collisions() {
        // 12 names all truncating to "FOOB" -- tests the 0-9 then A-Z suffix.
        let names: Vec<&str> = (0..12).map(|_| "FOOBAR").collect();
        let result = compress_names(names);
        assert_eq!(result[0], *b"FOOB"); // first keeps truncated name
        assert_eq!(result[1], *b"FOO0");
        assert_eq!(result[2], *b"FOO1");
        assert_eq!(result[9], *b"FOO8");
        assert_eq!(result[10], *b"FOO9");
        assert_eq!(result[11], *b"FOOA"); // 10 -> 'A'

        // All must be unique.
        let set: alloc::collections::BTreeSet<[u8; 4]> = result.iter().copied().collect();
        assert_eq!(set.len(), 12);
    }

    /// Test that tock_field_entries correctly compresses long field names
    /// and the resulting FieldEntry values have valid 4-byte names.
    #[test]
    fn test_tock_field_entries_long_names() {
        register_bitfields! [u32,
            LONG_NAMES_REG [
                UARTEN OFFSET(0) NUMBITS(1) [],
                UARTCLK OFFSET(1) NUMBITS(1) [],
                FIFO_ENABLE OFFSET(4) NUMBITS(1) [],
                FIFO_RESET OFFSET(5) NUMBITS(1) [],
                SHORT OFFSET(8) NUMBITS(1) []
            ]
        ];

        let entries = tock_field_entries::<u32, LONG_NAMES_REG::Register>(32);

        // Collect named entries.
        let named: Vec<([u8; 4], usize)> = entries
            .iter()
            .filter_map(|e| match e {
                FieldEntry::Named(n, b) => Some((*n, *b)),
                _ => None,
            })
            .collect();

        assert_eq!(named.len(), 5);

        // UARTEN and UARTCLK collide on "UART" -> first keeps UART, second gets UAR0.
        assert_eq!(named[0].0, *b"UART");
        assert_eq!(named[1].0, *b"UAR0");
        // FIFO_ENABLE and FIFO_RESET collide on "FIFO" -> FIFO and FIF0.
        assert_eq!(named[2].0, *b"FIFO");
        assert_eq!(named[3].0, *b"FIF0");
        // SHORT fits in 4 chars -> SHOR.
        assert_eq!(named[4].0, *b"SHOR");

        // All names must be unique.
        let name_set: alloc::collections::BTreeSet<[u8; 4]> =
            named.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            name_set.len(),
            named.len(),
            "all compressed names must be unique"
        );
    }
}
