//! RXM100CB revision-0x30 PCIe policy (SP3 package).
//!
//! OEM patcher 0xffd6814f, group builder 0xffd67b60, descriptor stride
//! 0x1b0. Zero physical-slot fields are left unspecified, not invented.
//!
//! The straps select a card per socket-0 group ([`Layout`]); the user can
//! override the bifurcation of a PCIe card's group ([`KNOBS`]), which keeps
//! the card's redriver setting and splits its x16 link.

use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::hotplug::{Hotplug, HotplugKind};
use fstart_driver_amd::sp3::dxio::lanes::{
    Bifurcation, Engine, EngineKind, Group, Lanes, PcieEngine, SataEngine, Speed, validate,
};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_options::Availability;

use crate::i2c::{Detected, RedriverMode};

// Mixed-card and socket-1 SATA descriptors +0x58 share this terminated
// PHY parameter list (ffd70ffc / ffd6fd14 / ffd6bb30).
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
    Capacity,
}

#[derive(Debug)]
pub struct Plan {
    pub engines: heapless::Vec<Engine<'static>, MAX_ENGINES>,
    /// Socket-0 P0..P3 redriver banks behind switches 0x70..0x73.
    pub redrivers: [RedriverMode; 4],
    /// Requested Ethernet descriptors omitted by the vendor's Milan policy.
    pub disabled_xgbe: u8,
}

/// Group, input-word shift, unbifurcated device number, physical slot.
const GROUPS: [(Group, u16, u8, u16); 4] = [
    (Group::P0, 0, 3, 1),
    (Group::P1, 8, 1, 3),
    (Group::P2, 12, 1, 4),
    (Group::P3, 4, 3, 2),
];

/// Whether `selector` is the mixed SATA/Ethernet card on `group`.
const fn mixed(group: Group, selector: u8) -> bool {
    matches!(selector, 8 | 9) && matches!(group, Group::P0 | Group::P1)
}

/// The socket-0 PCIe layout: the card each group's straps select, and the
/// user's bifurcation of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The 4-bit strap selector of P0..P3.
    pub selectors: [u8; 4],
    /// The link width a group is split into; `None` keeps the card's.
    pub bifurcation: [Option<Bifurcation>; 4],
    /// The I/O fuse removes socket 0's P1 and socket 1's P0.
    pub limited_io: bool,
}

impl Layout {
    /// The four slots before detection, for the knobs' build-time records:
    /// every group offered, Auto. The boot hides a group the straps or the
    /// I/O fuse remove.
    pub const UNDETECTED: Self = Self {
        selectors: [0; 4],
        bifurcation: [None; 4],
        limited_io: false,
    };

    /// The layout the straps select on revision 0x30. Other revisions
    /// have different socket-1 and forced-x8 wiring: do not apply this
    /// policy to an unknown board.
    pub fn new(detected: Detected, limited_io: bool) -> Result<Self, Error> {
        if detected.board_id != 0x30 {
            return Err(Error::Board(detected.board_id));
        }
        Ok(Self {
            selectors: GROUPS.map(|(_, shift, ..)| ((detected.straps >> shift) & 0xf) as u8),
            bifurcation: [None; 4],
            limited_io,
        })
    }

    /// Whether group `index` can be bifurcated.
    const fn available(&self, index: usize) -> Availability {
        let group = GROUPS[index].0;
        if self.limited_io && matches!(group, Group::P1) {
            Availability::Hidden("this processor's I/O fuse removes the group")
        } else if mixed(group, self.selectors[index]) {
            Availability::Hidden("the straps select the SATA/Ethernet card")
        } else {
            Availability::Editable
        }
    }
}

/// A group's bifurcation knob: its value is the link width; 0 (Auto)
/// keeps the card's links.
macro_rules! group_knob {
    ($name:ident, $index:literal, $key:literal, $label:literal) => {
        fstart_options::knob! {
            pub $name: Layout = Bifurcation::meta($key, $label, true);
            get(l) => match l.bifurcation[$index] {
                Some(b) => b.width() as u32,
                None => 0,
            };
            set(l, v) => l.bifurcation[$index] = Bifurcation::from_width(v);
            available(l) => l.available($index);
        }
    };
}

group_knob!(P0, 0, "ethanolx_p0_bifurcation", "P0 (slot 1) bifurcation");
group_knob!(P1, 1, "ethanolx_p1_bifurcation", "P1 (slot 3) bifurcation");
group_knob!(P2, 2, "ethanolx_p2_bifurcation", "P2 (slot 4) bifurcation");
group_knob!(P3, 3, "ethanolx_p3_bifurcation", "P3 (slot 2) bifurcation");

fstart_options::option_set!(
    /// The bifurcation of the four socket-0 slots: Auto by default, hidden
    /// at boot where the straps or fuses remove the group.
    pub static KNOBS: [Layout = Layout::UNDETECTED] = [P0, P1, P2, P3]
);

/// Whether the layout's engines are valid on the SoC.
pub fn check(layout: &Layout) -> Result<(), &'static str> {
    let plan = plan(layout).map_err(|_| "too many DXIO engines")?;
    validate(Generation::Milan, &plan.engines).map_err(|_| "the lane map does not validate")
}

fn group_ports(
    group: Group,
    selector: u8,
    device: u8,
    slot: u16,
    bifurcation: Option<Bifurcation>,
) -> heapless::Vec<Engine<'static>, 6> {
    if mixed(group, selector) {
        // The OEM supplies flag 0x20 only for P0/P1. There, ffd67b60
        // selects SATA 0..3, Ethernet 4..7 and (selector 8) upper PCIe x8.
        let sata = Engine::sata(0, Lanes::sp3(group, 0, 4), SataEngine::new(0)).tuning(SATA_TUNING);
        let ethernet = (4..8).map(|lane| Engine::ethernet(0, Lanes::sp3(group, lane, 1)));
        let upper = (selector == 8).then(|| {
            Engine::pcie(
                0,
                Lanes::sp3(group, 8, 8),
                PcieEngine::new().turn_off_unused(),
            )
        });
        return core::iter::once(sata)
            .chain(ethernet)
            .chain(upper)
            .collect();
    }
    // Without flag 0x20, selectors 8/9 take the ordinary reversed-x16
    // fallback, not a mixed layout on P2/P3.
    let ordinary = selector & 8 == 0 || matches!(selector, 8 | 9);
    let (width, reverse, hotplug) = if ordinary {
        (16, true, false)
    } else {
        match selector & 7 {
            2 | 4 => (4, false, true),
            3 | 5 => (8, false, false),
            _ => (16, false, false),
        }
    };
    if let Some(bifurcation) = bifurcation {
        // The card's x16 link, in its lane order, split as the user chose.
        let lanes = Lanes::sp3(group, 0, 16);
        let lanes = if reverse { lanes.reversed() } else { lanes };
        let p = PcieEngine::new().turn_off_unused().devfn(device, 1);
        let p = if ordinary { p.slot(slot) } else { p };
        let x16 = Engine::pcie(0, lanes, p);
        return if bifurcation == Bifurcation::X16 {
            core::iter::once(x16).collect()
        } else {
            x16.split(bifurcation.width()).collect()
        };
    }
    (0u8..16)
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
        .collect()
}

pub fn plan(layout: &Layout) -> Result<Plan, Error> {
    let limited_io = layout.limited_io;
    let mut engines = heapless::Vec::new();
    let mut redrivers = [RedriverMode::Off; 4];
    let mut disabled_xgbe = 0;
    for (i, (group, _, device, slot)) in GROUPS.into_iter().enumerate() {
        if limited_io && group == Group::P1 {
            continue;
        }
        let selector = layout.selectors[i];
        redrivers[i] = match selector {
            8 if matches!(group, Group::P0 | Group::P1) => RedriverMode::SataEthernet,
            0xa => RedriverMode::Pcie,
            _ => RedriverMode::Off,
        };
        for engine in group_ports(group, selector, device, slot, layout.bifurcation[i]) {
            // OEM ffd67825 removes Ethernet entries when the xGbE PCDs
            // 148..14f are false (all false in this Milan image). Do not
            // enable MACs merely because the card straps request them.
            if matches!(engine.kind, EngineKind::Ethernet) {
                disabled_xgbe += 1;
            } else {
                engines.push(engine).map_err(|_| Error::Capacity)?;
            }
        }
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
    Ok(Plan {
        engines,
        redrivers,
        disabled_xgbe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::dxio::lanes::place;

    fn plan_of(detected: Detected, limited_io: bool) -> Result<Plan, Error> {
        Layout::new(detected, limited_io).and_then(|layout| plan(&layout))
    }

    #[test]
    fn bench_straps_cover_both_sockets_and_native_nvme_hotplug() {
        let p = plan_of(
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
        assert_eq!(p.redrivers, [RedriverMode::Off; 4]);
        assert_eq!(p.disabled_xgbe, 0);
    }

    #[test]
    fn pcie_modes_validate_without_lane_overlap() {
        for selector in 0u16..16 {
            let result = plan_of(
                Detected {
                    board_id: 0x30,
                    straps: selector * 0x1111,
                },
                false,
            );
            let p = result.unwrap();
            validate(Generation::Milan, &p.engines).unwrap();
            let profile = match selector {
                8 => RedriverMode::SataEthernet,
                0xa => RedriverMode::Pcie,
                _ => RedriverMode::Off,
            };
            let expected = if selector == 8 {
                [
                    RedriverMode::SataEthernet,
                    RedriverMode::SataEthernet,
                    RedriverMode::Off,
                    RedriverMode::Off,
                ]
            } else {
                [profile; 4]
            };
            assert_eq!(p.redrivers, expected);
            assert_eq!(
                p.disabled_xgbe,
                if matches!(selector, 8 | 9) { 8 } else { 0 }
            );
        }
    }

    #[test]
    fn mixed_cards_keep_sata_ethernet_and_pcie_distinct() {
        for (group, shift) in [(Group::P0, 0), (Group::P1, 8)] {
            for selector in [8, 9] {
                let raw = group_ports(group, selector, 3, 1, None);
                validate(Generation::Milan, &raw).unwrap();
                assert!(matches!(raw[0].kind, EngineKind::Sata(_)));
                assert_eq!((raw[0].lanes.first(), raw[0].lanes.count()), (0, 4));
                assert_eq!(raw[0].tuning, SATA_TUNING);
                for absolute in [false, true] {
                    let mut out = [0; 1024];
                    fstart_driver_amd::sp3::dxio::engine::encode_tuning(
                        raw[0].tuning,
                        place(Generation::Milan, &raw[0]).unwrap().wire,
                        absolute,
                        &mut out,
                    )
                    .unwrap();
                }
                for (index, eth) in raw[1..5].iter().enumerate() {
                    assert_eq!(eth.kind, EngineKind::Ethernet);
                    assert_eq!((eth.lanes.first(), eth.lanes.count()), (4 + index as u8, 1));
                    assert!(eth.pcie_config().is_none());
                }
                if selector == 8 {
                    assert_eq!(raw.len(), 6);
                    assert!(raw[5].pcie_config().is_some());
                    assert_eq!((raw[5].lanes.first(), raw[5].lanes.count()), (8, 8));
                } else {
                    assert_eq!(raw.len(), 5);
                }
                let p = plan_of(
                    Detected {
                        board_id: 0x30,
                        straps: (selector as u16) << shift,
                    },
                    false,
                )
                .unwrap();
                validate(Generation::Milan, &p.engines).unwrap();
                assert_eq!(p.disabled_xgbe, 4);
                let bank = if group == Group::P0 { 0 } else { 1 };
                assert_eq!(
                    p.redrivers,
                    core::array::from_fn(|i| {
                        if selector == 8 && i == bank {
                            RedriverMode::SataEthernet
                        } else {
                            RedriverMode::Off
                        }
                    })
                );
                assert!(p.engines.iter().any(|e| e.socket == 0
                    && matches!(e.kind, EngineKind::Sata(_))
                    && e.lanes == Lanes::sp3(group, 0, 4)));
                assert!(!p.engines.iter().any(|e| e.kind == EngineKind::Ethernet));
                assert_eq!(
                    p.engines
                        .iter()
                        .filter(|e| e.socket == 0 && matches!(e.kind, EngineKind::Sata(_)))
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn mixed_selectors_on_p2_p3_use_the_vendor_x16_fallback() {
        for (group, shift) in [(Group::P2, 12), (Group::P3, 4)] {
            for selector in [8, 9] {
                let raw = group_ports(group, selector, 3, 1, None);
                assert_eq!(raw.len(), 1);
                assert!(raw[0].pcie_config().is_some());
                assert_eq!(raw[0].lanes.count(), 16);
                assert!(raw[0].lanes.is_reversed());
                let p = plan_of(
                    Detected {
                        board_id: 0x30,
                        straps: (selector as u16) << shift,
                    },
                    false,
                )
                .unwrap();
                assert_eq!(p.disabled_xgbe, 0);
                assert_eq!(p.redrivers, [RedriverMode::Off; 4]);
                validate(Generation::Milan, &p.engines).unwrap();
            }
        }
    }

    #[test]
    fn harvested_io_omits_the_two_vendor_gated_groups() {
        let p = plan_of(
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
            plan_of(
                Detected {
                    board_id: 0x31,
                    straps: 0
                },
                false
            ),
            Err(Error::Board(0x31))
        ));
    }

    #[test]
    fn bifurcation_overrides_split_the_cards_link_and_keep_its_redriver() {
        let bench = Detected {
            board_id: 0x30,
            straps: 0xa << 12,
        };
        for width in [16u8, 8, 4] {
            let mut layout = Layout::new(bench, false).unwrap();
            let all = Bifurcation::from_width(width.into());
            layout.bifurcation = [all; 4];
            check(&layout).unwrap();
            let p = plan(&layout).unwrap();
            // P2's NVMe card keeps its PCIe redriver profile.
            assert_eq!(p.redrivers[2], RedriverMode::Pcie);
            let p3: std::vec::Vec<_> = p
                .engines
                .iter()
                .filter(|e| {
                    e.socket == 0
                        && matches!(
                            e.lanes,
                            Lanes::Sp3 {
                                group: Group::P3,
                                ..
                            }
                        )
                })
                .collect();
            assert_eq!(p3.len(), usize::from(16 / width));
            assert!(p3.iter().all(|e| e.lanes.count() == width));
            // The ordinary slot is reversed: the highest lanes come first.
            if width < 16 {
                assert!(p3[0].lanes.first() > p3[1].lanes.first());
            }
        }
    }

    #[test]
    fn mixed_cards_and_fused_groups_are_not_offered() {
        let layout = Layout::new(
            Detected {
                board_id: 0x30,
                straps: 0x8,
            },
            true,
        )
        .unwrap();
        let offered: std::vec::Vec<_> = (0..4).map(|i| layout.available(i)).collect();
        assert!(matches!(offered[0], Availability::Hidden(_)));
        assert!(matches!(offered[1], Availability::Hidden(_)));
        assert_eq!(offered[2..], [Availability::Editable; 2]);
        let mut source_layout = layout;
        let mut report = fstart_options::Report::new();
        struct All(u32);
        impl fstart_options::Source for All {
            fn name(&self) -> &'static str {
                "all"
            }
            fn read(&self, _: &str) -> Result<Option<u32>, fstart_options::SourceError> {
                Ok(Some(self.0))
            }
        }
        fstart_options::resolve_checked(&KNOBS, &All(4), &mut source_layout, &mut report, check);
        let four = Some(Bifurcation::X4x4x4x4);
        assert_eq!(source_layout.bifurcation, [None, None, four, four]);
    }
}
