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
pub const PCIE: [fstart_driver_amd::mpio::session::Config<'static>; 11] = {
    use fstart_driver_amd::mpio::{rs::Port, session::Config};
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

/// Public Genoa MpioInitFlow.c GpioSlotResetControl: GPIO26 and remote
/// GPIO266, control value00C40000. Use common FCH register/bitfield access;
/// do not substitute Siena's GPIO135/136 or change unrelated pads.
pub fn release_perst<B: fstart_driver_amd::fch::Bus>(
    fch: &fstart_driver_amd::fch::Fch<B>,
) -> Result<(), &'static str> {
    use fstart_driver_amd::fch::{AcpiReg, gpio::Control, gpio_bank};
    use tock_registers::interfaces::{Readable, Writeable};
    const LOCAL: AcpiReg<u32, Control::Register> = gpio_bank::at(26 * 4);
    const REMOTE: AcpiReg<u32, Control::Register> = AcpiReg::new(0x1228);
    let value = Control::DRIVE_STRENGTH.val(2) + Control::OUTPUT::SET + Control::OUTPUT_ENABLE::SET;
    for reg in [LOCAL, REMOTE] {
        if fch.reg(reg).get() == u32::MAX {
            return Err("Onyx PERST GPIO unavailable");
        }
    }
    for reg in [LOCAL, REMOTE] {
        fch.reg(reg).set(value.value);
        if !fch.reg(reg).matches_all(value) {
            return Err("Onyx PERST GPIO readback failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usb_policy_has_valid_scalars_without_unverified_phy_coefficients() {
        use fstart_driver_amd::fch::songshan::usb::{ColdBoot, Interface};
        assert!(ColdBoot::for_interface(&USB, Interface::Absolute).is_ok());
        assert_eq!(USB.over_current, [0x1010; 2]);
        assert!(USB.usb3.is_none());
    }
    #[test]
    fn perst_writes_only_the_two_onyx_outputs_and_preflights_both() {
        use fstart_driver_amd::fch::{Acpimmio, Fch, Space, testing::World};
        let world = World::default();
        let fch = Fch::new(&world, Acpimmio::DIE0);
        assert_eq!(release_perst(&fch), Ok(()));
        assert_eq!(
            world.writes(),
            [
                (Space::Mmio, 0xfed8_1568, 4, 0x00c4_0000),
                (Space::Mmio, 0xfed8_1228, 4, 0x00c4_0000),
            ]
        );
        world.clear_log();
        world.set(Space::Mmio, 0xfed8_1228, 4, u32::MAX);
        assert!(release_perst(&fch).is_err());
        assert!(world.writes().is_empty());
    }
    #[test]
    fn cold_policy_uses_physical_lane_translation_not_reference_domains() {
        use fstart_driver_amd::mpio::{rs, session::Prepared};
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
