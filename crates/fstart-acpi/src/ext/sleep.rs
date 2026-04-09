//! AML Sleep and Stall operations.
//!
//! - [`Sleep`] -- suspends execution for the given milliseconds.
//! - [`Stall`] -- busy-waits for the given microseconds (max 100 us).
//!
//! These are commonly used in firmware ACPI methods for device
//! initialization sequences and power state transitions.

use acpi_tables::{Aml, AmlSink};

/// AML opcode constants.
const EXT_OP_PREFIX: u8 = 0x5B;
const SLEEP_OP: u8 = 0x22;
const STALL_OP: u8 = 0x21;

/// AML `Sleep` operation -- suspends execution for the specified duration.
///
/// The operand is a TermArg evaluating to the sleep duration in
/// milliseconds.  Any AML type implementing [`Aml`] can be used:
/// integer literals, `Local(n)`, `Arg(n)`, method calls, etc.
///
/// # AML encoding
///
/// ```text
/// ExtOpPrefix (0x5B) SleepOp (0x22) TermArg
/// ```
pub struct Sleep<'a> {
    msec_time: &'a dyn Aml,
}

impl<'a> Sleep<'a> {
    /// Create a Sleep operation with the given millisecond duration.
    pub fn new(msec_time: &'a dyn Aml) -> Self {
        Self { msec_time }
    }
}

impl Aml for Sleep<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(EXT_OP_PREFIX);
        sink.byte(SLEEP_OP);
        self.msec_time.to_aml_bytes(sink);
    }
}

/// AML `Stall` operation -- busy-waits for the specified microsecond
/// duration.
///
/// Per the ACPI specification, the operand must evaluate to at most
/// 100 microseconds.  For longer delays, use [`Sleep`] instead.
///
/// # AML encoding
///
/// ```text
/// ExtOpPrefix (0x5B) StallOp (0x21) TermArg
/// ```
pub struct Stall<'a> {
    usec_time: &'a dyn Aml,
}

impl<'a> Stall<'a> {
    /// Create a Stall operation with the given microsecond duration.
    pub fn new(usec_time: &'a dyn Aml) -> Self {
        Self { usec_time }
    }
}

impl Aml for Stall<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(EXT_OP_PREFIX);
        sink.byte(STALL_OP);
        self.usec_time.to_aml_bytes(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    #[test]
    fn test_sleep_literal() {
        let duration = 100u32;
        let sleep = Sleep::new(&duration);

        let mut bytes = Vec::new();
        sleep.to_aml_bytes(&mut bytes);

        // ExtOpPrefix + SleepOp + encoded u32
        assert_eq!(bytes[0], EXT_OP_PREFIX);
        assert_eq!(bytes[1], SLEEP_OP);
        assert!(bytes.len() > 2);
    }

    #[test]
    fn test_stall_literal() {
        let duration = 50u8;
        let stall = Stall::new(&duration);

        let mut bytes = Vec::new();
        stall.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], EXT_OP_PREFIX);
        assert_eq!(bytes[1], STALL_OP);
        assert!(bytes.len() > 2);
    }

    #[test]
    fn test_sleep_with_local_var() {
        let local0 = acpi_tables::aml::Local(0);
        let sleep = Sleep::new(&local0);

        let mut bytes = Vec::new();
        sleep.to_aml_bytes(&mut bytes);

        assert_eq!(bytes[0], EXT_OP_PREFIX);
        assert_eq!(bytes[1], SLEEP_OP);
        // Local0 encodes as 0x60
        assert_eq!(bytes[2], 0x60);
    }
}
