//! AML logical operations.
//!
//! - [`LAnd`] -- logical AND of two boolean operands.
//! - [`LOr`] -- logical OR of two boolean operands.
//! - [`LNot`] -- logical NOT of a boolean operand.
//!
//! These are used in `If`, `While`, and other predicates to combine
//! comparison results.

use acpi_tables::{Aml, AmlSink};

/// AML opcode constants.
const LAND_OP: u8 = 0x90;
const LOR_OP: u8 = 0x91;
const LNOT_OP: u8 = 0x92;

/// AML `LAnd` operation -- logical AND.
///
/// Returns `True` if both operands evaluate to non-zero, `False`
/// otherwise.
///
/// # AML encoding
///
/// ```text
/// LAndOp (0x90) Operand Operand
/// ```
pub struct LAnd<'a> {
    a: &'a dyn Aml,
    b: &'a dyn Aml,
}

impl<'a> LAnd<'a> {
    /// Create a logical AND of two operands.
    pub fn new(a: &'a dyn Aml, b: &'a dyn Aml) -> Self {
        Self { a, b }
    }
}

impl Aml for LAnd<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(LAND_OP);
        self.a.to_aml_bytes(sink);
        self.b.to_aml_bytes(sink);
    }
}

/// AML `LOr` operation -- logical OR.
///
/// Returns `True` if either operand evaluates to non-zero, `False`
/// otherwise.
///
/// # AML encoding
///
/// ```text
/// LOrOp (0x91) Operand Operand
/// ```
pub struct LOr<'a> {
    a: &'a dyn Aml,
    b: &'a dyn Aml,
}

impl<'a> LOr<'a> {
    /// Create a logical OR of two operands.
    pub fn new(a: &'a dyn Aml, b: &'a dyn Aml) -> Self {
        Self { a, b }
    }
}

impl Aml for LOr<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(LOR_OP);
        self.a.to_aml_bytes(sink);
        self.b.to_aml_bytes(sink);
    }
}

/// AML `LNot` operation -- logical NOT.
///
/// Returns `True` if the operand evaluates to zero, `False` otherwise.
///
/// # AML encoding
///
/// ```text
/// LNotOp (0x92) Operand
/// ```
pub struct LNot<'a> {
    operand: &'a dyn Aml,
}

impl<'a> LNot<'a> {
    /// Create a logical NOT of the given operand.
    pub fn new(operand: &'a dyn Aml) -> Self {
        Self { operand }
    }
}

impl Aml for LNot<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(LNOT_OP);
        self.operand.to_aml_bytes(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    #[test]
    fn test_land() {
        let a = 1u8;
        let b = 1u8;
        let land = LAnd::new(&a, &b);

        let mut bytes = Vec::new();
        land.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], LAND_OP);
        assert!(bytes.len() > 1);
    }

    #[test]
    fn test_lor() {
        let a = 0u8;
        let b = 1u8;
        let lor = LOr::new(&a, &b);

        let mut bytes = Vec::new();
        lor.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], LOR_OP);
        assert!(bytes.len() > 1);
    }

    #[test]
    fn test_lnot() {
        let a = 0u8;
        let lnot = LNot::new(&a);

        let mut bytes = Vec::new();
        lnot.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], LNOT_OP);
        assert!(bytes.len() > 1);
    }

    #[test]
    fn test_land_with_locals() {
        let local0 = acpi_tables::aml::Local(0);
        let local1 = acpi_tables::aml::Local(1);
        let land = LAnd::new(&local0, &local1);

        let mut bytes = Vec::new();
        land.to_aml_bytes(&mut bytes);

        assert_eq!(bytes, &[LAND_OP, 0x60, 0x61]);
    }

    #[test]
    fn test_lnot_of_local() {
        let local0 = acpi_tables::aml::Local(0);
        let lnot = LNot::new(&local0);

        let mut bytes = Vec::new();
        lnot.to_aml_bytes(&mut bytes);

        assert_eq!(bytes, &[LNOT_OP, 0x60]);
    }
}
