//! RXM100CB revision-0x30 PCIe policy (SP3 package).
//!
//! OEM patcher 0xffd6814f, group builder 0xffd67b60, descriptor stride
//! 0x1b0. Zero physical-slot fields are left unspecified, not invented.

use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::hotplug::{Hotplug, HotplugKind};
use fstart_driver_amd::sp3::dxio::lanes::{Engine, Group, Lanes, PcieEngine, SataEngine, Speed};

use crate::i2c::Detected;

// Socket 1 SATA descriptor +0x58: the vendor's terminated PHY parameter list.
const SATA_TUNING: &[(u8, u8)] = &[
    (0xc1, 0x00),
    (0xc2, 0x0e),
    (0xc0, 0x00),
    (0xd2, 0x01),
    (0xd3, 0x01),
    (0x81, 0x04),
    (0x82, 0x00),
    (0x80, 0x00),
    (0x92, 0x01),
    (0x93, 0x01),
    (0x41, 0x0a),
    (0x42, 0x00),
    (0x40, 0x00),
    (0x52, 0x01),
    (0x53, 0x01),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Board(u8),
    /// These selectors request SATA + xGbE, not PCIe bifurcation.
    Combination {
        group: Group,
        selector: u8,
    },
    Capacity,
}

#[derive(Debug)]
pub struct Plan {
    pub engines: heapless::Vec<Engine<'static>, MAX_ENGINES>,
    /// Socket-0 P0..P3 redriver banks behind switches 0x70..0x73.
    pub redrivers: [bool; 4],
}

fn group_ports(
    group: Group,
    selector: u8,
    device: u8,
    slot: u16,
) -> Result<heapless::Vec<Engine<'static>, 4>, Error> {
    let ordinary = selector & 8 == 0;
    let (width, reverse, hotplug) = if ordinary {
        (16, true, false)
    } else {
        match selector & 7 {
            2 | 4 => (4, false, true),
            3 | 5 => (8, false, false),
            6 | 7 => (16, false, false),
            _ => return Err(Error::Combination { group, selector }),
        }
    };
    Ok((0u8..16)
        .step_by(usize::from(width))
        .map(|first| {
            let lanes = Lanes::sp3(group, first, width);
            let lanes = if reverse { lanes.reversed() } else { lanes };
            let p = PcieEngine::new().turn_off_unused();
            let p = if width == 16 { p.devfn(device, 1) } else { p };
            let p = if ordinary { p.slot(slot) } else { p };
            let p = if hotplug {
                p.with_hotplug(Hotplug::new(HotplugKind::EnterpriseSsd))
            } else {
                p
            };
            Engine::pcie(0, lanes, p)
        })
        .collect())
}

pub fn plan(detected: Detected, limited_io: bool) -> Result<Plan, Error> {
    // Other reference revisions have different socket-1 and forced-x8 wiring.
    // Do not apply revision 0x30's outputs/lane policy to an unknown board.
    if detected.board_id != 0x30 {
        return Err(Error::Board(detected.board_id));
    }
    let mut engines = heapless::Vec::new();
    let mut redrivers = [false; 4];
    // Group, input-word shift, unbifurcated device number, physical slot.
    let groups = [
        (Group::P0, 0, 3, 1),
        (Group::P1, 8, 1, 3),
        (Group::P2, 12, 1, 4),
        (Group::P3, 4, 3, 2),
    ];
    for (i, (group, shift, device, slot)) in groups.into_iter().enumerate() {
        if limited_io && group == Group::P1 {
            continue;
        }
        let selector = ((detected.straps >> shift) & 0xf) as u8;
        redrivers[i] = selector == 0xa;
        engines
            .extend_from_slice(&group_ports(group, selector, device, slot)?)
            .map_err(|_| Error::Capacity)?;
    }
    // The bench BMC's dedicated WAFL link; its devfn is silicon-defined.
    engines
        .push(Engine::pcie(
            0,
            Lanes::sp3(Group::Wafl, 1, 1),
            PcieEngine::new().max_speed(Speed::Gen1).turn_off_unused(),
        ))
        .map_err(|_| Error::Capacity)?;
    engines
        .push(Engine::sata(1, Lanes::sp3(Group::P1, 0, 8), SataEngine::new(0)).tuning(SATA_TUNING))
        .map_err(|_| Error::Capacity)?;
    engines
        .push(Engine::pcie(
            1,
            Lanes::sp3(Group::P3, 0, 16),
            PcieEngine::new().devfn(3, 1).turn_off_unused(),
        ))
        .map_err(|_| Error::Capacity)?;
    if !limited_io {
        // OEM template at 0xffd6f654: four forward x4 NVMe ports, dev3 fn1..4,
        // LinkHotplug=5, no external SMU/I2C slot signaling.
        for index in 0..4 {
            engines
                .push(Engine::pcie(
                    1,
                    Lanes::sp3(Group::P0, index * 4, 4),
                    PcieEngine::new()
                        .devfn(3, index + 1)
                        .turn_off_unused()
                        .with_hotplug(Hotplug::new(HotplugKind::EnterpriseSsd)),
                ))
                .map_err(|_| Error::Capacity)?;
        }
    }
    Ok(Plan { engines, redrivers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::dxio::lanes::{place, validate};
    use fstart_driver_amd::sp3::soc::Generation;

    #[test]
    fn bench_straps_cover_both_sockets_and_native_nvme_hotplug() {
        let p = plan(
            Detected {
                board_id: 0x30,
                straps: 0,
            },
            false,
        )
        .unwrap();
        validate(Generation::Milan, &p.engines).unwrap();
        assert_eq!(p.engines.iter().filter(|e| e.socket == 0).count(), 5);
        assert_eq!(p.engines.iter().filter(|e| e.socket == 1).count(), 6);
        assert_eq!(
            place(Generation::Milan, &p.engines[0]).unwrap().phy,
            (15, 0)
        );
        let bmc = place(Generation::Milan, &p.engines[4]).unwrap();
        assert_eq!((bmc.nbio, bmc.phy), (0, (129, 129)));
        assert_eq!(
            p.engines
                .iter()
                .filter_map(Engine::pcie_config)
                .filter(|p| p.hotplug.is_some())
                .count(),
            4
        );
        assert_eq!(p.redrivers, [false; 4]);
    }

    #[test]
    fn pcie_modes_validate_without_lane_overlap() {
        for selector in 0u16..16 {
            let result = plan(
                Detected {
                    board_id: 0x30,
                    straps: selector * 0x1111,
                },
                false,
            );
            if matches!(selector, 8 | 9) {
                assert!(matches!(result, Err(Error::Combination { .. })));
            } else {
                let p = result.unwrap();
                validate(Generation::Milan, &p.engines).unwrap();
                assert_eq!(p.redrivers, [selector == 0xa; 4]);
            }
        }
    }

    #[test]
    fn harvested_io_omits_the_two_vendor_gated_groups() {
        let p = plan(
            Detected {
                board_id: 0x30,
                straps: 0,
            },
            true,
        )
        .unwrap();
        validate(Generation::Milan, &p.engines).unwrap();
        assert_eq!(p.engines.len(), 6);
        assert!(
            p.engines
                .iter()
                .all(|e| e.pcie_config().is_none_or(|p| p.hotplug.is_none()))
        );
    }

    #[test]
    fn other_board_revisions_require_their_own_policy() {
        assert!(matches!(
            plan(
                Detected {
                    board_id: 0x31,
                    straps: 0
                },
                false
            ),
            Err(Error::Board(0x31))
        ));
    }
}
