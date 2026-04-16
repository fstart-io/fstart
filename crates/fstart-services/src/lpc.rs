//! Low Pin Count (LPC) bus service.
//!
//! Implemented by southbridge drivers that expose an LPC bus with
//! programmable decode ranges for ISA-style peripherals (SuperIO,
//! TPM, embedded controller).
//!
//! Children attached to this bus specify their LPC config address via
//! [`crate::device::BusDevice`]; their driver opens the appropriate
//! decode range by asking the southbridge via this trait.

use crate::ServiceError;

/// Bus provider for LPC-attached peripherals.
pub trait LpcBus: Send + Sync {
    /// Program an LPC I/O decode range.
    ///
    /// `index` selects one of the four Generic I/O Decode Range
    /// registers (LPC_IOD1–4 on ICH; device-specific encoding).
    /// `value` is the 32-bit register content (base, size mask, enable).
    fn set_decode_range(&mut self, index: u8, value: u32) -> Result<(), ServiceError>;
}
