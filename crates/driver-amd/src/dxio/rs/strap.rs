// SPDX-License-Identifier: MIT OR Apache-2.0
//! The straps of a PCIe core, which MPIO writes for the host (request
//! 0Ch, [`super::session::Session::write_strap`]) between the map and the
//! configure phases; configure latches them into the core's ports.
//!
//! A strap is addressed by its index and the core's instance:
//! `instance << 16 | index`, the instance being the IOS plus four times
//! the wrapper, at most 9 (Siena AmdNbioPei `ffcf97fd`, openSIL
//! `WritePcieStrap`, MpioPcie.c:415). Genoa A0/A1 numbered the straps
//! from A8h on two lower; the RS revisions this crate knows
//! ([`crate::ccx::rs::Revision`]) all use the numbers below. Their names
//! are Oxide's (`sys/io/genoa/pcie_rsmu.h`); openSIL's
//! `MpioPcieStraps.h` has the same numbers with the names redacted.

/// The core has an AER capability (single bit, reset 0): without it the
/// ports have none and the OS cannot see their errors (openSIL
/// `SIL_RESERVED_838`, from `CfgAEREnable`).
pub const AER_EN: u16 = 0x06;
/// ECRC generation and checking capability (reset 0); the enables in the
/// AER capability stay the OS's.
pub const ECRC_GEN_EN: u16 = 0x4c;
pub const ECRC_CHECK_EN: u16 = 0x4d;
/// Completer-abort errors are reported (reset 1).
pub const CPL_ABORT_ERR_EN: u16 = 0x5f;

/// What the RS vendors write on every PCIe core, for the AER capability
/// of its ports: AER, completer aborts and ECRC capability on (Siena
/// AmdNbioPei `ffcf3247`, the S8040's V2.00, with its wrapper defaults:
/// AER and ECRC generation and checking, which its Linux sees on every
/// root port; openSIL `MpioCfgWrapperBeforeBifurcation` with Onyx's
/// `CONFIG_AER_ENABLE` and `CONFIG_PCIE_ECRC_ENABLEMENT`).
pub const AER: [(u16, u32); 4] = [
    (AER_EN, 1),
    (CPL_ABORT_ERR_EN, 1),
    (ECRC_GEN_EN, 1),
    (ECRC_CHECK_EN, 1),
];

/// The highest core instance; the RS vendors clamp to it.
const LAST_INSTANCE: u8 = 9;

/// Request 0Ch's first argument for strap `index` of `wrapper` on IOS
/// `ios`.
pub const fn argument(ios: u8, wrapper: u8, index: u16) -> u32 {
    let instance = ios + 4 * wrapper;
    let instance = if instance > LAST_INSTANCE {
        LAST_INSTANCE
    } else {
        instance
    };
    (instance as u32) << 16 | index as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_follow_the_vendor() {
        assert_eq!(argument(0, 0, AER_EN), 0x0000_0006);
        assert_eq!(argument(3, 1, AER_EN), 0x0007_0006);
        assert_eq!(argument(0, 2, ECRC_GEN_EN), 0x0008_004c);
        // IOS 2's bonus wrapper is the last instance.
        assert_eq!(argument(2, 2, ECRC_CHECK_EN), 0x0009_004d);
    }
}
