// SPDX-License-Identifier: MIT OR Apache-2.0
//! GIGABYTE MD25-CG0 (1P SP7) PCIe lane map and PERST# wiring.
//!
//! Verified from the MD25-CG0 `F01` firmware `$A26` socket-0 MPIO table
//! (10 descriptors, stride `0x156`): six PCIe 6.0 x16 slots/risers and four
//! bonus-lane links (AST2700 BMC VGA on lane 129, onboard LAN1 on lanes
//! 130..=131, and two x2 M.2 / auxiliary links on lanes 132..=135).

use fstart_driver_amd::fch::qilian::{self, Perst};
use fstart_platform_amd::sp7::LaneDescriptor;

/// AST2700 BMC VGA lane (trained by PSP/ABL).
pub const BMC_VGA_LANE: u8 = 129;

/// Links already trained by PSP/ABL prior to x86 MPIO bring-up.
pub const PSP_TRAINED: [(u8, u8); 1] = [(BMC_VGA_LANE, BMC_VGA_LANE)];

/// Common PCIe PERST# output (`Qilian` FCH pad 26, drive strength 2).
pub const PERST: [Perst; 1] = [qilian::PERST_COMMON];

/// Socket-0 PCIe lane descriptors from the MD25-CG0 `$A26` board table.
pub const PCIE: [LaneDescriptor; 10] = [
    // IOD0 PCIe 6.0 x16 / MCIO group 0
    LaneDescriptor {
        start_lane: 0,
        end_lane: 15,
        max_gen: 6,
        psp_trained: false,
    },
    // IOD0 PCIe 6.0 x16 / MCIO group 1
    LaneDescriptor {
        start_lane: 16,
        end_lane: 31,
        max_gen: 6,
        psp_trained: false,
    },
    // IOD0 PCIe 6.0 x16 / MCIO group 2
    LaneDescriptor {
        start_lane: 32,
        end_lane: 47,
        max_gen: 6,
        psp_trained: false,
    },
    // IOD1 PCIe 6.0 x16 / MCIO group 0
    LaneDescriptor {
        start_lane: 64,
        end_lane: 79,
        max_gen: 6,
        psp_trained: false,
    },
    // IOD1 PCIe 6.0 x16 / MCIO group 1
    LaneDescriptor {
        start_lane: 80,
        end_lane: 95,
        max_gen: 6,
        psp_trained: false,
    },
    // IOD1 PCIe 6.0 x16 / MCIO group 2
    LaneDescriptor {
        start_lane: 96,
        end_lane: 111,
        max_gen: 6,
        psp_trained: false,
    },
    // Bonus lane: AST2700 BMC VGA PCIe x1 (PSP-trained)
    LaneDescriptor {
        start_lane: 129,
        end_lane: 129,
        max_gen: 4,
        psp_trained: true,
    },
    // Bonus lanes: onboard LAN1 PCIe x2
    LaneDescriptor {
        start_lane: 130,
        end_lane: 131,
        max_gen: 4,
        psp_trained: false,
    },
    // Bonus lanes: onboard M.2 slot 0 PCIe x2
    LaneDescriptor {
        start_lane: 132,
        end_lane: 133,
        max_gen: 5,
        psp_trained: false,
    },
    // Bonus lanes: onboard M.2 slot 1 PCIe x2
    LaneDescriptor {
        start_lane: 134,
        end_lane: 135,
        max_gen: 5,
        psp_trained: false,
    },
];

/// Qilian USB cold-boot policy: both xHCI controllers enabled, active-low
/// over-current, sparse mode enabled, default PHY tuning (no unverified PHY
/// coefficients copied).
pub const USB: fstart_driver_amd::fch::qilian::usb::Policy =
    fstart_driver_amd::fch::qilian::usb::Policy {
        enabled: [true, true],
        over_current: [0x1010, 0x1010],
        device_removable: 0,
        ras: false,
        over_current_low: true,
        sparse: true,
        debug_pipe_switch: false,
        force_gen1: 0,
        usb2: None,
        usb3: None,
    };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcie_lanes_are_sorted_non_overlapping_and_match_widths() {
        let mut prev_end: Option<u8> = None;
        let mut total_lanes = 0u16;
        for desc in PCIE {
            assert!(desc.end_lane >= desc.start_lane);
            if let Some(prev) = prev_end {
                assert!(desc.start_lane > prev, "overlapping descriptor");
            }
            prev_end = Some(desc.end_lane);
            total_lanes += desc.width();
        }
        // 6 × 16 + 1 + 2 + 2 + 2 = 103 lanes
        assert_eq!(total_lanes, 103);
        let bmc = PCIE.iter().find(|d| d.start_lane == BMC_VGA_LANE).unwrap();
        assert!(bmc.psp_trained);
        assert_eq!(bmc.width(), 1);
    }

    #[test]
    fn usb_policy_validates_without_unverified_phy_tuning() {
        use fstart_driver_amd::fch::qilian::usb::ColdBoot;
        assert!(ColdBoot::new(&USB).is_ok());
        assert!(USB.usb2.is_none());
        assert!(USB.usb3.is_none());
    }
}
