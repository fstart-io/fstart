//! AML Increment and Decrement operations.
//!
//! - [`Increment`] -- adds 1 to a named object or local variable.
//! - [`Decrement`] -- subtracts 1 from a named object or local variable.
//!
//! These are commonly used in loop constructs within ACPI methods.

use acpi_tables::{Aml, AmlSink};

/// AML opcode constants.
const INCREMENT_OP: u8 = 0x75;
const DECREMENT_OP: u8 = 0x76;

/// AML `Increment` operation -- adds 1 to the target.
///
/// The target is a SuperName (named object, `Local(n)`, or `Arg(n)`).
/// The result is stored back in the target and also returned as the
/// expression result.
///
/// # AML encoding
///
/// ```text
/// IncrementOp (0x75) SuperName
/// ```
pub struct Increment<'a> {
    target: &'a dyn Aml,
}

impl<'a> Increment<'a> {
    /// Create an Increment operation on the given target.
    pub fn new(target: &'a dyn Aml) -> Self {
        Self { target }
    }
}

impl Aml for Increment<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(INCREMENT_OP);
        self.target.to_aml_bytes(sink);
    }
}

/// AML `Decrement` operation -- subtracts 1 from the target.
///
/// The target is a SuperName (named object, `Local(n)`, or `Arg(n)`).
/// The result is stored back in the target and also returned as the
/// expression result.
///
/// # AML encoding
///
/// ```text
/// DecrementOp (0x76) SuperName
/// ```
pub struct Decrement<'a> {
    target: &'a dyn Aml,
}

impl<'a> Decrement<'a> {
    /// Create a Decrement operation on the given target.
    pub fn new(target: &'a dyn Aml) -> Self {
        Self { target }
    }
}

impl Aml for Decrement<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(DECREMENT_OP);
        self.target.to_aml_bytes(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    #[test]
    fn test_increment_local() {
        let local0 = acpi_tables::aml::Local(0);
        let inc = Increment::new(&local0);

        let mut bytes = Vec::new();
        inc.to_aml_bytes(&mut bytes);

        assert_eq!(bytes, &[INCREMENT_OP, 0x60]); // 0x60 = Local0
    }

    #[test]
    fn test_decrement_local() {
        let local1 = acpi_tables::aml::Local(1);
        let dec = Decrement::new(&local1);

        let mut bytes = Vec::new();
        dec.to_aml_bytes(&mut bytes);

        assert_eq!(bytes, &[DECREMENT_OP, 0x61]); // 0x61 = Local1
    }

    #[test]
    fn test_increment_named() {
        let name = acpi_tables::aml::Path::new("CNT_");
        let inc = Increment::new(&name);

        let mut bytes = Vec::new();
        inc.to_aml_bytes(&mut bytes);

        // IncrementOp + 4-byte NameSeg
        assert_eq!(bytes[0], INCREMENT_OP);
        assert_eq!(&bytes[1..5], b"CNT_");
    }
}
