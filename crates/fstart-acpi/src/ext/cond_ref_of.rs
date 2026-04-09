//! AML CondRefOf and RefOf operations.
//!
//! - [`CondRefOf`] -- conditional reference: returns `True` if the
//!   named object exists, storing a reference to it in the target.
//! - [`RefOf`] -- creates a reference to a named object.
//!
//! `CondRefOf` is commonly used in `_OSC` and `_DSM` methods to check
//! for optional ACPI objects before accessing them.

use acpi_tables::{Aml, AmlSink};

/// AML opcode constants.
const EXT_OP_PREFIX: u8 = 0x5B;
const COND_REF_OF_OP: u8 = 0x12;
const REF_OF_OP: u8 = 0x71;

/// AML `CondRefOf` operation -- conditional reference to a named object.
///
/// Returns `True` if the named object exists.  When it exists, a
/// reference to it is stored in `target`.  The target is typically
/// a `Local(n)` variable.
///
/// # AML encoding
///
/// ```text
/// ExtOpPrefix (0x5B) CondRefOfOp (0x12) SuperName Target
/// ```
///
/// # Example use case
///
/// In an `_OSC` method, checking whether a device supports a feature:
///
/// ```ignore
/// // If (CondRefOf(FEAT, Local0)) { ... }
/// let feat = Path::new("FEAT");
/// let local0 = Local(0);
/// let cond = CondRefOf::new(&feat, &local0);
/// ```
pub struct CondRefOf<'a> {
    source: &'a dyn Aml,
    target: &'a dyn Aml,
}

impl<'a> CondRefOf<'a> {
    /// Create a CondRefOf checking `source` and storing into `target`.
    pub fn new(source: &'a dyn Aml, target: &'a dyn Aml) -> Self {
        Self { source, target }
    }
}

impl Aml for CondRefOf<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(EXT_OP_PREFIX);
        sink.byte(COND_REF_OF_OP);
        self.source.to_aml_bytes(sink);
        self.target.to_aml_bytes(sink);
    }
}

/// AML `RefOf` operation -- creates a reference to a named object.
///
/// Returns an ObjectReference to the specified SuperName.  Often used
/// with `DeRefOf` to dereference.
///
/// # AML encoding
///
/// ```text
/// RefOfOp (0x71) SuperName
/// ```
pub struct RefOf<'a> {
    source: &'a dyn Aml,
}

impl<'a> RefOf<'a> {
    /// Create a RefOf for the given named object.
    pub fn new(source: &'a dyn Aml) -> Self {
        Self { source }
    }
}

impl Aml for RefOf<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(REF_OF_OP);
        self.source.to_aml_bytes(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    #[test]
    fn test_cond_ref_of() {
        let feat = acpi_tables::aml::Path::new("FEAT");
        let local0 = acpi_tables::aml::Local(0);
        let cond = CondRefOf::new(&feat, &local0);

        let mut bytes = Vec::new();
        cond.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], EXT_OP_PREFIX);
        assert_eq!(bytes[1], COND_REF_OF_OP);
        assert_eq!(&bytes[2..6], b"FEAT");
        assert_eq!(bytes[6], 0x60); // Local0
    }

    #[test]
    fn test_ref_of() {
        let name = acpi_tables::aml::Path::new("DEV0");
        let ref_of = RefOf::new(&name);

        let mut bytes = Vec::new();
        ref_of.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], REF_OF_OP);
        assert_eq!(&bytes[1..5], b"DEV0");
    }
}
