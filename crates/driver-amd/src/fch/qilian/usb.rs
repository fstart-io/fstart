// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's xHCIs: Songshan's SMU message stream
//! ([`crate::fch::songshan::usb`]) with Qilian's two differences, identical
//! in the Dell PR9XJ and GIGABYTE MD25-CG0 SilVenicePei builds
//! (`FchQlXhciPassParameter 0xf32390`, FCH.md §1.5, §2.7) [I]:
//!
//! - the USB3 PHY 4-field tuning register is `0x12_00A4` (space 2), not
//!   `0x12_0088` (the [`Usb3Tuning`] fields named after Songshan's
//!   register keep their meaning);
//! - the debug-pipe switch goes as one operation `F5h` message per
//!   controller, its controller bit inverted
//!   ([`super::super::songshan::usb::DebugPipe::PerController`]); the
//!   policy's one switch is sent to both.
//!
//! The messages go to the SMU of the FCH's IOD (`UsbInit 0x0A`,
//! `ConfigUpdate 0x13`); only socket 0 and socket 1's die-0 FCHs have USB
//! ([`super::secondary::has_usb`]).

pub use crate::fch::songshan::usb::{
    ColdBoot, DebugPipe, Error, Policy, Usb2Tuning, Usb3Tuning, Version,
};

/// Qilian's USB3 PHY 4-field tuning register (space 2).
pub const USB3_TUNE: u32 = 0x12_00a4;

/// Qilian's version of the xHCI messages.
pub const QILIAN: Version = Version {
    usb3_tune: USB3_TUNE,
    debug_pipe: DebugPipe::PerController,
};

/// Validate a Qilian cold-boot message stream for `policy`.
pub fn cold_boot(policy: &Policy) -> Result<ColdBoot, Error> {
    ColdBoot::for_version(policy, &QILIAN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Smu;
    use crate::smn::testing::SmnSpace;

    fn policy() -> Policy {
        let tune = Usb3Tuning {
            word_1242d8: 7,
            bit_12c064: false,
            nibble_12c064: 0,
            word_13004c: 7,
            bit_120088: true,
            mid_120088: 5,
            low_bit_120088: false,
            low_120088: 5,
        };
        Policy {
            enabled: [true, true],
            over_current: [0xf0f0, 0xffff],
            device_removable: 0,
            ras: false,
            over_current_low: false,
            sparse: false,
            debug_pipe_switch: true,
            force_gen1: 0,
            usb2: None,
            usb3: Some([[tune; 2]; 2]),
        }
    }

    #[test]
    fn debug_pipe_per_controller_and_the_qilian_phy_register() {
        let space = SmnSpace::default();
        let smu = Smu::default();
        cold_boot(&policy())
            .unwrap()
            .run(&space.smn(), &smu)
            .unwrap();
        let calls = smu.0.borrow();
        // F5h for controller 0 carries controller bit 1, and vice versa.
        assert_eq!(calls[0], (0x13, [0, 0, 0, 0xf510_0001, 0, 0]));
        assert_eq!(calls[1], (0x13, [0, 0, 0, 0xf500_0001, 0, 0]));
        assert!(calls.iter().all(|c| c.1[3] >> 24 != 0xd0));
        let tune: Vec<_> = calls.iter().filter(|c| c.1[0] == USB3_TUNE).collect();
        assert_eq!(tune.len(), 4, "two ports of two controllers");
        assert!(calls.iter().all(|c| c.1[0] != 0x12_0088));
        assert_eq!(calls.last().unwrap(), &(0x0a, [3, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn songshan_is_unchanged() {
        let space = SmnSpace::default();
        let smu = Smu::default();
        ColdBoot::new(&policy())
            .unwrap()
            .run(&space.smn(), &smu)
            .unwrap();
        let calls = smu.0.borrow();
        assert_eq!(calls[0], (0x13, [0, 0, 0, 0xd000_0001, 0, 0]));
        assert!(calls.iter().any(|c| c.1[0] == 0x12_0088));
        assert!(calls.iter().all(|c| c.1[0] != USB3_TUNE));
    }
}
