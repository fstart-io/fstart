// SPDX-License-Identifier: MIT OR Apache-2.0
//! GIGABYTE MD25-CG0 (1P SP7) PCIe ports and PERST# wiring.
//!
//! The ports are the MD25-CG0 `F01` firmware's `$A26` socket-0 MPIO table
//! (`GbtAMDCpmTableInit`, 10 descriptors, stride `0x156`, decoded in
//! `reports/research/MPIO/cpm_tables_gigabyte.json`): six x16 ports and four
//! bonus-core links. Every descriptor has PERST group 1, a device present,
//! unused lanes off and ASPM off; the requested devfns and speeds below are
//! its port parameters. The x16 ports on core 0 (lanes 32-47 and 96-111)
//! ask for 1.1, which SP7 B0 refuses as the vendor does (3.1 stays).

use fstart_driver_amd::fch::qilian::{self, Perst};
use fstart_platform_amd::sp7::{Engine, PortDesc, SocketTopology, Speed};

/// The lane of the link the ABL trains before x86
/// (`PcdEarlyBmcLinkLaneNum` `0x80`; the F01 APCB's EarlyPcieConfig marks
/// lane 128 present, Gen4, 5.1). IOD 1's bonus core, not a `$A26` port.
pub const EARLY_BMC_LANE: u8 = 0x80;

/// The AST2700's display: the ABL-trained link. GIGABYTE's SMBIOS type 41
/// (`GbtSmbiosType41`, onboard video "Ast2700") finds it at device 5
/// function 1 of IOD 1's root bus, and only when no `$A26` PCIe port
/// covers lane 128 (its `0x14a8` scans the table for lanes 128..131 [I];
/// RESIDUAL.md §6).
pub const BMC_VGA_LANE: u8 = EARLY_BMC_LANE;

/// `$A26`'s x1 port at lane 129 (5.2, Gen3): not the AST2700's display
/// ([`BMC_VGA_LANE`]); the device behind it is not identified (the F01
/// APCB's early PCIe entry has lane 129 not present).
pub const LANE_129: u8 = 129;

/// Common PCIe PERST# output (`Qilian` FCH pad 26, drive strength 2), released
/// on both the primary FCH and every verified secondary FCH aperture.
pub const PERST: [Perst; 1] = [qilian::PERST_COMMON];

/// Descriptor 49 (`GbtLanCtrl` writes its engine type from Setup byte
/// `0x3AE`, LAN1 enable).
pub const LAN1: usize = 7;

/// Socket 0's ports with onboard LAN1 on or off.
pub const fn ports(lan1: bool) -> [PortDesc<'static>; 10] {
    let mut ports = [
        PortDesc::pcie(0, 15).devfn(3, 1),
        PortDesc::pcie(16, 31).devfn(3, 1),
        PortDesc::pcie(32, 47).devfn(1, 1),
        PortDesc::pcie(64, 79).devfn(3, 1),
        PortDesc::pcie(80, 95).devfn(3, 1),
        PortDesc::pcie(96, 111).devfn(1, 1),
        // x1, Gen3 capability and target (device not identified).
        PortDesc::pcie(LANE_129, LANE_129)
            .devfn(5, 2)
            .speed(Speed::Gen3, Speed::Gen3),
        // Onboard LAN1 x2.
        PortDesc::pcie(130, 131).devfn(5, 1),
        // Bonus-core x2 links, Gen4 capability and target (devices not
        // identified).
        PortDesc::pcie(132, 133)
            .devfn(5, 2)
            .speed(Speed::Gen4, Speed::Gen4),
        PortDesc::pcie(134, 135)
            .devfn(5, 1)
            .speed(Speed::Gen4, Speed::Gen4),
    ];
    if !lan1 {
        ports[LAN1].engine = Engine::Unused;
    }
    ports
}

/// Socket 0's ports, LAN1 on (the Setup default).
pub const PCIE: [PortDesc<'static>; 10] = ports(true);
const PCIE_NO_LAN1: [PortDesc<'static>; 10] = ports(false);

/// The board's MPIO topology.
pub const fn topology(lan1: bool) -> &'static [SocketTopology<'static>] {
    const ON: [SocketTopology<'static>; 1] = [SocketTopology {
        socket: 0,
        ports: &PCIE,
    }];
    const OFF: [SocketTopology<'static>; 1] = [SocketTopology {
        socket: 0,
        ports: &PCIE_NO_LAN1,
    }];
    if lan1 { &ON } else { &OFF }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_sorted_non_overlapping_and_on_their_iods() {
        use fstart_driver_amd::dxio::wh::Layout;
        let mut prev_end: Option<u8> = None;
        let mut total_lanes = 0u16;
        for desc in PCIE {
            let (lo, hi) = desc.lanes();
            if let Some(prev) = prev_end {
                assert!(lo > prev, "overlapping descriptor");
            }
            prev_end = Some(hi);
            total_lanes += u16::from(hi - lo) + 1;
            desc.validate().unwrap();
        }
        assert_eq!(total_lanes, 103);
        let bmc = PCIE.iter().find(|d| d.start_lane == LANE_129).unwrap();
        let map = bmc.mapping(Layout::Sp7B0).unwrap();
        assert_eq!(
            (map.iod, map.core, map.start, map.reversed),
            (1, 6, 66, true)
        );
        // The display is the ABL's link, not an MPIO port.
        assert!(PCIE.iter().all(|d| {
            let (lo, hi) = d.lanes();
            !(lo..=hi).contains(&BMC_VGA_LANE)
        }));
        assert_eq!(ports(false)[LAN1].engine, Engine::Unused);
        assert_eq!(topology(false)[0].ports[LAN1].start_lane, 130);
    }
}
