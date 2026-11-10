//! AML Break operation.
//!
//! [`Break`] terminates execution of the innermost enclosing `While`
//! loop.  It is the AML equivalent of the C `break` statement.

use acpi_tables::{Aml, AmlSink};

/// AML opcode for Break.
const BREAK_OP: u8 = 0xA5;

/// AML `Break` operation -- exits the innermost `While` loop.
///
/// # AML encoding
///
/// ```text
/// BreakOp (0xA5)
/// ```
pub struct Break;

impl Aml for Break {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(BREAK_OP);
    }
}
