//! AML `Divide` operation.
//!
//! `acpi_tables` does not model `DivideOp` (it has four operands: dividend,
//! divisor, remainder target, quotient target), so the `acpi_dsl!` emitter
//! uses this object for the `/` operator. The remainder target is typically
//! `Zero` (discard); the quotient target is a caller-provided temporary
//! `Name` whose value the emitter then reads back.

extern crate alloc;

use acpi_tables::{Aml, AmlSink};

const DIVIDE_OP: u8 = 0x7e;

/// AML Divide operation: `remainder = dividend % divisor`,
/// `quotient = dividend / divisor`.
pub struct Divide<'a> {
    dividend: &'a dyn Aml,
    divisor: &'a dyn Aml,
    remainder: &'a dyn Aml,
    quotient: &'a dyn Aml,
}

impl<'a> Divide<'a> {
    /// Create a Divide operation. Pass `fstart_acpi::aml::Zero` as
    /// `remainder` to discard the modulo result.
    pub fn new(
        dividend: &'a dyn Aml,
        divisor: &'a dyn Aml,
        remainder: &'a dyn Aml,
        quotient: &'a dyn Aml,
    ) -> Self {
        Self {
            dividend,
            divisor,
            remainder,
            quotient,
        }
    }
}

impl Aml for Divide<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(DIVIDE_OP);
        self.dividend.to_aml_bytes(sink);
        self.divisor.to_aml_bytes(sink);
        self.remainder.to_aml_bytes(sink);
        self.quotient.to_aml_bytes(sink);
    }
}
