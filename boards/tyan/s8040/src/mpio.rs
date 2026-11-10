// SPDX-License-Identifier: MIT OR Apache-2.0
//! Active S8040GM2NE lane map and normal IFR connector choices.
//! OemPei ffda926c / board-tables-v200.json, callbacks at ffda8c8c.
//! NFX V0.02's complete lane descriptors match after relocating the SATA
//! ancillary pointer; its three pointed type7 values match byte-for-byte.
//! No X710 descriptor, no second socket and no legacy M.2 SATA detection.

use fstart_driver_amd::mpio::rs::{Kind, LinkSpeed, Port};
use heapless::Vec;

/// Maximum with all four slots x4x4x4x4, MCIO0/1 four x4 and MCIO2/3
/// two x4, both M.2, SATA and three onboard x1 links.
pub const MAX_ENGINES: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bifurcation16 {
    X16,
    X8X8,
    X4X4X4X4,
}
impl Bifurcation16 {
    fn width(self) -> u8 {
        match self {
            Self::X16 => 16,
            Self::X8X8 => 8,
            Self::X4X4X4X4 => 4,
        }
    }
    /// Siena IFR IDs: deliberately NOT SP3's enum values.
    pub const fn setup_id(self) -> u8 {
        match self {
            Self::X16 => 1,
            Self::X8X8 => 3,
            Self::X4X4X4X4 => 6,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bifurcation8 {
    X8,
    X4X4,
}
impl Bifurcation8 {
    fn width(self) -> u8 {
        match self {
            Self::X8 => 8,
            Self::X4X4 => 4,
        }
    }
    pub const fn setup_id(self) -> u8 {
        match self {
            Self::X8 => 8,
            Self::X4X4 => 9,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// PCIE#1..4 (physical slot order, not the vendor's descriptor order).
    pub slots: [Bifurcation16; 4],
    pub mcio01: Bifurcation16,
    pub mcio23: Bifurcation8,
    /// PCIE#1..4, MCIO0/1, MCIO2/3. Auto or Gen1..5.
    pub speed: [LinkSpeed; 6],
    pub compliance: bool,
    pub lan: [bool; 2],
    pub vga: bool,
}
impl Options {
    pub const DEFAULT: Self = Self {
        slots: [Bifurcation16::X16; 4],
        mcio01: Bifurcation16::X4X4X4X4,
        mcio23: Bifurcation8::X4X4,
        speed: [LinkSpeed::Auto; 6],
        compliance: false,
        lan: [true; 2],
        vga: true,
    };
    pub fn slot_variable(&self) -> [u8; 13] {
        let mut bytes = [0; 13];
        for (i, bifurcation) in self.slots.iter().enumerate() {
            bytes[i] = bifurcation.setup_id();
        }
        bytes[4] = self.mcio01.setup_id();
        bytes[5] = self.mcio23.setup_id();
        for (i, speed) in self.speed.iter().enumerate() {
            bytes[6 + i] = *speed as u8;
        }
        bytes[12] = self.compliance.into();
        bytes
    }
}
impl Default for Options {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// BMC-link presence is a live input, not implied by enabling VGA in
/// Setup. The vendor firmware forces it false when its link PPI is missing.
pub struct Detected {
    pub bmc_link_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Slot(u8),
    /// Pair-level labels: individual MCIO connector order is not verified.
    Mcio01,
    Mcio23,
    /// Which lane pair is physical CN1/CN2 remains unverified.
    M2Pair(u8),
    Sata,
    Lan(u8),
    BmcVga,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    pub destination: Destination,
    pub port: Port,
    /// Firmware type7 ancillary values, not the LinkSpeed enum. The
    /// eventual ancillary owner must serialize them and supply ASK indices.
    pub speed_overrides: &'static [u32],
}

/// SATA92..95's three type7 entries. Verified identical in public V2.00
/// and the live NFX V0.02 OemPei; only the descriptor's pointer relocates.
const SATA_SPEED_OVERRIDES: [u32; 3] = [0x0000_1c07, 0x0000_2107, 0x0800_2807];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Capacity,
}

/// One socket's native engines. Topology and root-bus assignment belong
/// to the platform, NOT to this board map. No port is silently dropped.
pub fn engines(options: &Options, detected: &Detected) -> Result<Vec<Engine, MAX_ENGINES>, Error> {
    let mut engines = Vec::new();
    let mut add = |destination, mut port: Port, speed: Option<LinkSpeed>| {
        // Speed and compliance callbacks cover the six connector ranges,
        // not M.2, SATA, LAN or the BMC's Gen2 link.
        if let Some(speed) = speed {
            port.compliance = options.compliance;
            port.max_speed = speed;
            port.target_speed = speed;
        }
        engines
            .push(Engine {
                destination,
                port,
                speed_overrides: if port.kind == Kind::Sata {
                    &SATA_SPEED_OVERRIDES
                } else {
                    &[]
                },
            })
            .map_err(|_| Error::Capacity)
    };
    let width = options.mcio01.width();
    for i in 0..16 / width {
        add(
            Destination::Mcio01,
            Port {
                device: 1,
                function: 4 - i,
                ..Port::pcie(i * width, (i + 1) * width - 1)
            },
            Some(options.speed[4]),
        )?;
    }
    // Vendor order, including the slots whose remaining descriptor rows
    // become active only with bifurcation. Splits keep requested devfn=auto.
    for (slot, start) in [(3, 16), (1, 48), (2, 96)] {
        let width = options.slots[slot - 1].width();
        for i in 0..16 / width {
            add(
                Destination::Slot(slot as u8),
                Port::pcie(start + i * width, start + (i + 1) * width - 1),
                Some(options.speed[slot - 1]),
            )?;
        }
    }
    let width = options.mcio23.width();
    for i in 0..8 / width {
        add(
            Destination::Mcio23,
            Port {
                device: 3,
                function: i + 1,
                ..Port::pcie(80 + i * width, 80 + (i + 1) * width - 1)
            },
            Some(options.speed[5]),
        )?;
    }
    for (i, start) in [(0, 88), (1, 90)] {
        add(
            Destination::M2Pair(i),
            Port {
                device: 3,
                function: i + 3,
                ..Port::pcie(start, start + 1)
            },
            None,
        )?;
    }
    add(
        Destination::Sata,
        Port {
            kind: Kind::Sata,
            ..Port::pcie(92, 95)
        },
        None,
    )?;
    let width = options.slots[3].width();
    for i in 0..16 / width {
        add(
            Destination::Slot(4),
            Port::pcie(112 + i * width, 112 + (i + 1) * width - 1),
            Some(options.speed[3]),
        )?;
    }
    for i in 0..2 {
        add(
            Destination::Lan(i + 1),
            Port {
                present: options.lan[i as usize],
                target_speed: LinkSpeed::Gen2,
                ..Port::pcie(132 + i, 132 + i)
            },
            None,
        )?;
    }
    add(
        Destination::BmcVga,
        Port {
            present: options.vga && detected.bmc_link_present,
            target_speed: LinkSpeed::Gen2,
            ..Port::pcie(134, 134)
        },
        None,
    )?;
    Ok(engines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::mpio::rs;

    #[test]
    fn default_setup_and_lane_coverage() {
        assert_eq!(
            Options::DEFAULT.slot_variable(),
            [1, 1, 1, 1, 6, 9, 0, 0, 0, 0, 0, 0, 0]
        );
        let engines = engines(
            &Options::DEFAULT,
            &Detected {
                bmc_link_present: true,
            },
        )
        .unwrap();
        assert_eq!(engines.len(), 16);
        let mut lanes = [false; 135];
        for engine in &engines {
            for lane in engine.port.start..=engine.port.end {
                assert!(!lanes[lane as usize], "overlap at lane {lane}");
                lanes[lane as usize] = true;
            }
        }
        assert_eq!(lanes.iter().filter(|&&on| on).count(), 99);
        assert!(lanes[88..96].iter().all(|&on| on));
        assert!(lanes[32..48].iter().all(|&on| !on));
        assert!(engines.iter().all(|e| !e.port.early_train));
    }

    #[test]
    fn all_normal_bifurcations_fit_without_overlap_or_lost_ports() {
        let choices = [
            Bifurcation16::X16,
            Bifurcation16::X8X8,
            Bifurcation16::X4X4X4X4,
        ];
        // All 3^4 slot choices, independently of both MCIO choices: 486 maps.
        for mut configuration in 0..81 {
            let slots = core::array::from_fn(|_| {
                let choice = choices[configuration % 3];
                configuration /= 3;
                choice
            });
            for mcio01 in choices {
                for mcio23 in [Bifurcation8::X8, Bifurcation8::X4X4] {
                    let options = Options {
                        slots,
                        mcio01,
                        mcio23,
                        ..Options::DEFAULT
                    };
                    let engines = engines(
                        &options,
                        &Detected {
                            bmc_link_present: true,
                        },
                    )
                    .unwrap();
                    let mut lanes = [false; 135];
                    for engine in &engines {
                        rs::build(&[0, 1, 2, 3], &engine.port, 0).unwrap();
                        // IOS1 can be fused off. Every normal
                        // connector policy must also fit the actual roots.
                        rs::build(&[0, 2, 3], &engine.port, 0).unwrap();
                        for lane in engine.port.start..=engine.port.end {
                            assert!(!lanes[lane as usize]);
                            lanes[lane as usize] = true;
                        }
                    }
                    assert_eq!(lanes.iter().filter(|&&on| on).count(), 99);
                }
            }
        }
        let e = engines(
            &Options {
                slots: [Bifurcation16::X4X4X4X4; 4],
                ..Options::DEFAULT
            },
            &Detected {
                bmc_link_present: true,
            },
        )
        .unwrap();
        assert_eq!(e.len(), MAX_ENGINES);
    }

    #[test]
    fn nfx_sata_ancillary_values_are_retained_not_confused_with_speed_caps() {
        let ports = engines(
            &Options::DEFAULT,
            &Detected {
                bmc_link_present: true,
            },
        )
        .unwrap();
        let sata = ports
            .iter()
            .find(|e| e.destination == Destination::Sata)
            .unwrap();
        assert_eq!((sata.port.start, sata.port.end), (92, 95));
        assert_eq!(sata.speed_overrides, &[0x1c07, 0x2107, 0x0800_2807]);
        assert_eq!(
            ports
                .iter()
                .filter(|e| !e.speed_overrides.is_empty())
                .count(),
            1
        );
    }

    #[test]
    fn explicit_bmc_presence_lan_and_speed_policy() {
        let options = Options {
            lan: [false, true],
            speed: [LinkSpeed::Gen4; 6],
            compliance: true,
            ..Options::DEFAULT
        };
        let e = engines(
            &options,
            &Detected {
                bmc_link_present: false,
            },
        )
        .unwrap();
        assert!(
            !e.iter()
                .find(|e| e.destination == Destination::BmcVga)
                .unwrap()
                .port
                .present
        );
        assert!(
            !e.iter()
                .find(|e| e.destination == Destination::Lan(1))
                .unwrap()
                .port
                .present
        );
        assert!(
            e.iter()
                .find(|e| e.destination == Destination::Lan(2))
                .unwrap()
                .port
                .present
        );
        for e in &e {
            if matches!(
                e.destination,
                Destination::Slot(_) | Destination::Mcio01 | Destination::Mcio23
            ) {
                assert_eq!(
                    (e.port.target_speed, e.port.max_speed),
                    (LinkSpeed::Gen4, LinkSpeed::Gen4)
                );
                assert!(e.port.compliance);
            } else {
                assert!(!e.port.compliance);
            }
        }
    }
}
