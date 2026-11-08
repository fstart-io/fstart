// SPDX-License-Identifier: MIT OR Apache-2.0
//! AST2600 board wiring, S8040 V2.00 IMPLEMENTATION_REPORT §5.
//! The BMC owns fan/thermal policy. No AST2500 SCU recipe is applied.

use embedded_hal::delay::DelayNs;
use fstart_driver_ipmi::kcs::{self, Kcs};

/// The AST2600's COM1 (the BMC's SOL) interrupt over SERIRQ.
pub const COM1_IRQ: u8 = 4;
pub const KCS_PORT: u16 = kcs::DEFAULT_PORT;
pub const KCS_SPACING: u16 = kcs::DEFAULT_SPACING;
pub use kcs::DEFAULT_ACPI as IPMI_ACPI;

/// Required host I/O ranges from the AST2600 entry. These are requirements,
/// not allocations of SP3's WideIO slots. KCS separately needs ca2/ca3.
pub const DECODE_RANGES: &[(u16, u16)] = &[(0x2e, 2), (0xa00, 0x90)];

/// # Safety
/// Host decodes are enabled, the AST2600 is ready, and SIO ownership is
/// exclusive. COM1 base and J6/J7 UART routing must be established from
/// BMC/board state: the vendor firmware's early sequence programs neither.
pub unsafe fn early_init() {
    // SAFETY: caller owns the decoded BMC configuration ports.
    unsafe { fstart_driver_superio::aspeed_ast2600::early_init_at(0x2e) };
}

/// # Safety
/// KCS ca2/ca3 is decoded and exclusively owned. This does not claim a
/// specific eSPI/LPC transport configuration or open any FCH aperture.
pub unsafe fn kcs<D: DelayNs>(delay: D) -> Kcs<D> {
    unsafe { Kcs::new(KCS_PORT, KCS_SPACING, delay) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_resources() {
        assert_eq!((KCS_PORT, KCS_SPACING), (0xca2, 1));
        assert_eq!(IPMI_ACPI.port, 0xca2);
        assert_eq!(DECODE_RANGES, &[(0x2e, 2), (0xa00, 0x90)]);
    }
}
