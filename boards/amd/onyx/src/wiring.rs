// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lane facts from the public coreboot amd/onyx_poc devicetree. These are
//! reference board data only, NOT Siena ASK descriptors or a trained BDF map.
//! Root identity is bound separately to live DF routes. Cold-boot MPIO uses
//! the shared Rs wire format checked against Genoa's public MpioInitLib.h.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connector {
    pub name: &'static str,
    pub first_lane: u8,
    pub last_lane: u8,
    /// Domain label in the reference source, not a discovered PCI segment,
    /// bus, physical IOS index or fabric ID on the lab board.
    pub reference_domain: u8,
    pub server_hotplug: bool,
    pub early_bmc: bool,
}

pub const CONNECTORS: [Connector; 12] = [
    Connector {
        name: "P2",
        first_lane: 48,
        last_lane: 63,
        reference_domain: 0,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "G2",
        first_lane: 112,
        last_lane: 127,
        reference_domain: 0,
        server_hotplug: true,
        early_bmc: false,
    },
    Connector {
        name: "GPP128",
        first_lane: 128,
        last_lane: 131,
        reference_domain: 0,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "P3",
        first_lane: 16,
        last_lane: 31,
        reference_domain: 1,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "G3",
        first_lane: 80,
        last_lane: 95,
        reference_domain: 1,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "P1",
        first_lane: 32,
        last_lane: 47,
        reference_domain: 2,
        server_hotplug: true,
        early_bmc: false,
    },
    Connector {
        name: "G1",
        first_lane: 64,
        last_lane: 79,
        reference_domain: 2,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "P0",
        first_lane: 0,
        last_lane: 15,
        reference_domain: 3,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "G0",
        first_lane: 96,
        last_lane: 111,
        reference_domain: 3,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "WAFL",
        first_lane: 132,
        last_lane: 133,
        reference_domain: 3,
        server_hotplug: false,
        early_bmc: false,
    },
    Connector {
        name: "BMC",
        first_lane: 134,
        last_lane: 134,
        reference_domain: 3,
        server_hotplug: false,
        early_bmc: true,
    },
    Connector {
        name: "GPP135",
        first_lane: 135,
        last_lane: 135,
        reference_domain: 3,
        server_hotplug: false,
        early_bmc: false,
    },
];

/// Normal cold training only. The live factory ASK snapshot contains eleven
/// entries: lane134 is already PSP-trained and MUST NOT be reconfigured here.
/// Server-hotplug flags remain recorded above; their service is not enabled.
/// Device/function zero asks for the native port; MAP results, not domain
/// labels, select the physical remap slots.
pub const PCIE: [fstart_driver_amd::dxio::rs::session::Config<'static>; 11] = {
    use fstart_driver_amd::dxio::rs::{Port, session::Config};
    let mut out = [Config {
        port: Port::pcie(0, 0),
        phy: &[],
        speed: &[],
    }; 11];
    let mut i = 0;
    let mut n = 0;
    while i < CONNECTORS.len() {
        if !CONNECTORS[i].early_bmc {
            out[n].port = Port::pcie(CONNECTORS[i].first_lane, CONNECTORS[i].last_lane);
            n += 1;
        }
        i += 1;
    }
    assert!(n == out.len());
    out
};

/// The links the PSP trains before MPIO (the BMC's): their first and last
/// lane.
pub const PSP_TRAINED: [(u8, u8); 1] = {
    let mut out = [(0, 0); 1];
    let mut i = 0;
    let mut n = 0;
    while i < CONNECTORS.len() {
        if CONNECTORS[i].early_bmc {
            out[n] = (CONNECTORS[i].first_lane, CONNECTORS[i].last_lane);
            n += 1;
        }
        i += 1;
    }
    assert!(n == out.len());
    out
};

/// The first lane of the BMC's display link.
pub const BMC: u8 = PSP_TRAINED[0].0;
/// The lane of the onboard BCM5720 (GPP135).
pub const BCM5720: u8 = 135;

/// Genoa reference cold policy: each controller has two bonded ports;
/// USB2/USB3 OC nibbles0/1 become1010h, active low, all ports force Gen1.
/// Keep default PHY settings: the reference bridge clears Usb31PhyEnable
/// with its socket1 assignment; no unverified Siena PHY coefficients copied.
pub const USB: fstart_driver_amd::fch::songshan::usb::Policy =
    fstart_driver_amd::fch::songshan::usb::Policy {
        enabled: [true, true],
        over_current: [0x1010, 0x1010],
        device_removable: 0,
        ras: false,
        over_current_low: true,
        sparse: true,
        debug_pipe_switch: false,
        force_gen1: 0xf,
        usb2: None,
        usb3: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usb_policy_has_valid_scalars_without_unverified_phy_coefficients() {
        use fstart_driver_amd::fch::songshan::usb::ColdBoot;
        assert!(ColdBoot::new(&USB).is_ok());
        assert_eq!(USB.over_current, [0x1010; 2]);
        assert!(USB.usb3.is_none());
    }
    #[test]
    fn cold_policy_uses_physical_lane_translation_not_reference_domains() {
        use fstart_driver_amd::dxio::rs::{ask as rs, session::Prepared};
        let roots = [0, 1, 2, 3];
        assert!(Prepared::new(0, &roots, &PCIE).is_ok());
        assert_eq!(PCIE.len(), 11);
        assert!(PCIE.iter().all(|c| c.port.start != 134));
        let expected = [2, 2, 2, 3, 3, 1, 1, 0, 0, 0, 0];
        for (config, physical) in PCIE.iter().zip(expected) {
            let p = config.port;
            assert_eq!(
                rs::translate(&roots, p.start, p.end).unwrap().root_index,
                physical
            );
            assert_eq!(
                (p.device, p.function, p.hotplug, p.early_train),
                (0, 0, 0, false)
            );
        }
    }
    #[test]
    fn reference_lanes_cover_136_without_overlap_or_invented_bdfs() {
        let mut lanes = [false; 136];
        for c in CONNECTORS {
            assert!(c.reference_domain < 4);
            assert!(c.first_lane <= c.last_lane);
            let width = c.last_lane - c.first_lane + 1;
            assert!(width.is_power_of_two() && width <= 16);
            for lane in c.first_lane..=c.last_lane {
                assert!(!lanes[usize::from(lane)]);
                lanes[usize::from(lane)] = true;
            }
        }
        assert!(lanes.into_iter().all(|v| v));
        assert_eq!(CONNECTORS.iter().filter(|c| c.early_bmc).count(), 1);
        let bmc = CONNECTORS.iter().find(|c| c.early_bmc).unwrap();
        assert_eq!(
            (bmc.first_lane, bmc.last_lane, bmc.reference_domain),
            (134, 134, 3)
        );
        assert_eq!(
            CONNECTORS
                .iter()
                .filter(|c| c.server_hotplug)
                .map(|c| c.name)
                .collect::<std::vec::Vec<_>>(),
            ["G2", "P1"]
        );
    }
}
