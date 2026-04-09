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
use tock_registers::debug::RegisterDebugInfo;
use tock_registers::fields::Field as TockField;
use tock_registers::UIntLike;

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
    // disambiguate subsequent ones by replacing the last char with 0-9.
    let len = candidates.len();
    for i in 0..len {
        let mut suffix = 0u8;
        for j in (i + 1)..len {
            if candidates[j] == candidates[i] {
                // Collision: disambiguate candidates[j].
                let mut fixed = candidates[i];
                fixed[3] = b'0' + suffix;
                suffix += 1;
                // If the first occurrence hasn't been disambiguated yet
                // and we're on the first collision, also fix the later one.
                candidates[j] = fixed;
            }
        }
    }

    candidates
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
/// `ReadOnly<T, R>`.  Custom register wrappers (e.g., fstart-mmio's
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

        assert_eq!(entries[0], FieldEntry::Named([b'U', b'A', b'R', b'T'], 1));
        assert_eq!(entries[1], FieldEntry::Reserved(7));
        assert_eq!(entries[2], FieldEntry::Named(*b"TXE_", 1));
        assert_eq!(entries[3], FieldEntry::Named(*b"RXE_", 1));
        assert_eq!(entries[4], FieldEntry::Reserved(22));
    }

    #[test]
    fn test_tock_acpi_field_produces_aml() {
        let field =
            TockAcpiField::new::<u32, TEST_FR::Register>("FREG", OpRegionSpace::SystemMemory, 0x18);

        let mut bytes = Vec::new();
        field.to_aml_bytes(&mut bytes);

        // Should contain OpRegion opcode (0x5B 0x80) and Field opcode (0x5B 0x81)
        assert!(
            bytes.windows(2).any(|w| w == [0x5B, 0x80]),
            "expected OpRegion opcode"
        );
        assert!(
            bytes.windows(2).any(|w| w == [0x5B, 0x81]),
            "expected Field opcode"
        );
        // Should contain region name "FREG"
        assert!(bytes.windows(4).any(|w| w == b"FREG"));
        // Should contain field names
        assert!(bytes.windows(4).any(|w| w == b"RXFE"));
        assert!(bytes.windows(4).any(|w| w == b"TXFF"));
    }

    #[test]
    fn test_multi_register_field() {
        let fr_entries = tock_field_entries::<u32, TEST_FR::Register>(32);
        let cr_entries = tock_field_entries::<u32, TEST_CR::Register>(32);

        let combined = build_multi_register_field(
            "REGS",
            OpRegionSpace::SystemMemory,
            0x00,
            0x34,
            FieldAccessType::DWord,
            &[
                (0x18, 32, &fr_entries), // FR at offset 0x18
                (0x30, 32, &cr_entries), // CR at offset 0x30
            ],
        );

        let mut bytes = Vec::new();
        combined.to_aml_bytes(&mut bytes);

        assert!(bytes.windows(4).any(|w| w == b"REGS"));
        assert!(bytes.windows(4).any(|w| w == b"RXFE"));
        assert!(bytes.windows(4).any(|w| w == b"TXFF"));
        // CR fields should be present too (truncated to 4 chars)
        assert!(bytes.windows(4).any(|w| w == [b'U', b'A', b'R', b'T']));
        assert!(bytes.windows(4).any(|w| w == b"TXE_"));
        assert!(bytes.windows(4).any(|w| w == b"RXE_"));
    }

    #[test]
    fn test_tock_field_in_acpi_dsl() {
        // Demonstrates using TockAcpiField inside acpi_dsl! via #{} interpolation.
        let fr_field =
            TockAcpiField::new::<u32, TEST_FR::Register>("FREG", OpRegionSpace::SystemMemory, 0x18);

        let aml: Vec<u8> = fstart_acpi_macros::acpi_dsl! {
            device("UAR0") {
                name("_HID", "ARMH0011");
                name("_UID", 0u32);
                // OpRegion + Field derived from tock-registers definitions
                #{fr_field}
            }
        };

        // Device opcode
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
        // Contains device name and register fields
        assert!(aml.windows(4).any(|w| w == b"UAR0"));
        assert!(aml.windows(4).any(|w| w == b"FREG"));
        assert!(aml.windows(4).any(|w| w == b"RXFE"));
        assert!(aml.windows(4).any(|w| w == b"TXFF"));
    }

    // ---------------------------------------------------------------
    // x86 MCH (Memory Controller Hub) northbridge example.
    //
    // The register definitions below are the Rust equivalent of the
    // coreboot ASL `OperationRegion(MCHP, PCI_Config, 0x00, 0x100)`
    // with its Field bitfield layout.  The tock-registers bridge
    // produces identical ACPI AML from these definitions.
    // ---------------------------------------------------------------

    register_bitfields! [u32,
        /// EPBAR register at PCI config offset 0x40.
        MCH_EPBAR [
            EPEN OFFSET(0) NUMBITS(1) [],
            EPBR OFFSET(12) NUMBITS(20) []
        ],
        /// MCHBAR register at PCI config offset 0x44.
        MCH_MCHBAR [
            MHEN OFFSET(0) NUMBITS(1) [],
            MHBR OFFSET(14) NUMBITS(18) []
        ],
        /// PCIe BAR register at PCI config offset 0x48.
        MCH_PXBAR [
            PXEN OFFSET(0) NUMBITS(1) [],
            PXSZ OFFSET(1) NUMBITS(2) [],
            PXBR OFFSET(26) NUMBITS(6) []
        ],
        /// DMIBAR register at PCI config offset 0x4C.
        MCH_DMIBAR [
            DMEN OFFSET(0) NUMBITS(1) [],
            DMBR OFFSET(12) NUMBITS(20) []
        ]
    ];

    register_bitfields! [u8,
        /// PAM0 register at PCI config offset 0x90.
        MCH_PAM0 [
            PM0H OFFSET(4) NUMBITS(2) []
        ],
        /// PAM1 register at PCI config offset 0x91.
        MCH_PAM1 [
            PM1L OFFSET(0) NUMBITS(2) [],
            PM1H OFFSET(4) NUMBITS(2) []
        ],
        /// TOLUD register at PCI config offset 0x9C.
        MCH_TOLUD [
            TLUD OFFSET(3) NUMBITS(5) []
        ]
    ];

    register_bitfields! [u16,
        /// TOM register at PCI config offset 0xA0.
        MCH_TOM [
            TOM_ OFFSET(0) NUMBITS(16) []
        ]
    ];

    /// Full MCH northbridge test: define registers with tock-registers,
    /// combine them with build_multi_register_field, and produce a
    /// complete MCHC device using acpi_dsl!.
    ///
    /// This is the Rust equivalent of the coreboot ASL example:
    /// ```text
    /// Device (MCHC) {
    ///     Name(_ADR, 0x00000000)
    ///     OperationRegion(MCHP, PCI_Config, 0x00, 0x100)
    ///     Field (MCHP, DWordAcc, NoLock, Preserve) {
    ///         Offset (0x40), EPEN, 1, ...
    ///     }
    /// }
    /// ```
    #[test]
    fn test_mch_northbridge_from_tock_registers() {
        // Step 1: Convert tock-registers definitions to ACPI FieldEntry slices.
        let epbar = tock_field_entries::<u32, MCH_EPBAR::Register>(32);
        let mchbar = tock_field_entries::<u32, MCH_MCHBAR::Register>(32);
        let pxbar = tock_field_entries::<u32, MCH_PXBAR::Register>(32);
        let dmibar = tock_field_entries::<u32, MCH_DMIBAR::Register>(32);
        let pam0 = tock_field_entries::<u8, MCH_PAM0::Register>(8);
        let pam1 = tock_field_entries::<u8, MCH_PAM1::Register>(8);
        let tolud = tock_field_entries::<u8, MCH_TOLUD::Register>(8);
        let tom = tock_field_entries::<u16, MCH_TOM::Register>(16);

        // Step 2: Combine into a single ACPI Field spanning PCI config 0x00..0x100.
        let mchp = build_multi_register_field(
            "MCHP",
            OpRegionSpace::PCIConfig,
            0x00,
            0x100,
            FieldAccessType::DWord,
            &[
                (0x40, 32, &epbar),
                (0x44, 32, &mchbar),
                (0x48, 32, &pxbar),
                (0x4C, 32, &dmibar),
                (0x90, 8, &pam0),
                (0x91, 8, &pam1),
                (0x9C, 8, &tolud),
                (0xA0, 16, &tom),
            ],
        );

        // Step 3: Produce the MCHC device using acpi_dsl! with tock-derived fields.
        let aml: Vec<u8> = fstart_acpi_macros::acpi_dsl! {
            device("MCHC") {
                name("_ADR", 0x0000_0000u32);
                #{mchp}
            }
        };

        // Verify structure.
        assert_eq!(aml[0], 0x5B); // ExtOpPrefix
        assert_eq!(aml[1], 0x82); // DeviceOp
        assert!(aml.windows(4).any(|w| w == b"MCHC"), "MCHC device name");
        assert!(aml.windows(4).any(|w| w == b"MCHP"), "MCHP region name");

        // Verify OpRegion + Field opcodes.
        assert!(aml.windows(2).any(|w| w == [0x5B, 0x80]), "OpRegion opcode");
        assert!(aml.windows(2).any(|w| w == [0x5B, 0x81]), "Field opcode");

        // Verify key field names derived from tock-registers.
        assert!(aml.windows(4).any(|w| w == b"EPEN"), "EPBAR.EPEN field");
        assert!(aml.windows(4).any(|w| w == b"EPBR"), "EPBAR.EPBR field");
        assert!(aml.windows(4).any(|w| w == b"MHEN"), "MCHBAR.MHEN field");
        assert!(aml.windows(4).any(|w| w == b"MHBR"), "MCHBAR.MHBR field");
        assert!(aml.windows(4).any(|w| w == b"PXEN"), "PXBAR.PXEN field");
        assert!(aml.windows(4).any(|w| w == b"PXSZ"), "PXBAR.PXSZ field");
        assert!(aml.windows(4).any(|w| w == b"PXBR"), "PXBAR.PXBR field");
        assert!(aml.windows(4).any(|w| w == b"DMEN"), "DMIBAR.DMEN field");
        assert!(aml.windows(4).any(|w| w == b"DMBR"), "DMIBAR.DMBR field");
        assert!(aml.windows(4).any(|w| w == b"PM0H"), "PAM0.PM0H field");
        assert!(aml.windows(4).any(|w| w == b"PM1L"), "PAM1.PM1L field");
        assert!(aml.windows(4).any(|w| w == b"PM1H"), "PAM1.PM1H field");
        assert!(aml.windows(4).any(|w| w == b"TLUD"), "TOLUD.TLUD field");
        assert!(aml.windows(4).any(|w| w == b"TOM_"), "TOM.TOM_ field");
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
