// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH (Venice) dual-IOD MPIO v2 lane mapping and ASK session builder.
//!
//! Verified against openSIL `MpioInitFlowWh.c` (`0xf5ec00`), `f78d30.c` (52-byte
//! ASK entries, `DOORBELL_V2 = 0x0C91_0954`, `Encoding::LowByte`), and WH
//! `PcieCore` lane templates (`StartDxioLane == StartPhyLane` per IOD):
//! - Socket lanes `0..=63` -> IOD 0 local lanes `0..=63` (4 × 16-lane cores)
//! - Socket lanes `64..=127` -> IOD 1 local lanes `0..=63` (`lane - 64`)
//! - Socket bonus lanes `128..=131` -> IOD 0 local bonus lanes `128..=131`
//! - Socket bonus lanes `132..=135` -> IOD 1 local bonus lanes `128..=131` (`lane - 4`)

use heapless::Vec;

use super::rs::{
    ancillary::Ancillary,
    ask::{self, Ask, Mapping},
    mailbox::{DOORBELL_V2, Encoding},
    session::{Config, Error, MAX_PORTS, Prepared},
};

/// WH PCIe core lane windows within a single IOD (`DxioLane == PhyLane`).
/// `(core_id, first_lane, last_lane)`.
pub const IOD_CORES: [(u8, u8, u8); 5] = [
    (0, 0, 15),
    (1, 16, 31),
    (2, 32, 47),
    (3, 48, 63),
    (4, 128, 131), // Bonus core (P6)
];

/// Map a socket-wide WH lane pair `(start, end)` to `(iod, local_mapping)`.
///
/// Validates that both endpoints lie inside the same WH IOD and the same PCIe
/// core window, preserving lane reversal (`start > end`).
pub fn translate_socket_lane(start: u8, end: u8) -> Result<(u8, Mapping), ask::Error> {
    let map_one = |lane: u8| -> Option<(u8, u8)> {
        match lane {
            0..=63 => Some((0, lane)),
            64..=127 => Some((1, lane - 64)),
            128..=131 => Some((0, lane)),
            132..=135 => Some((1, lane - 4)),
            _ => None,
        }
    };
    let (iod_a, local_a) = map_one(start).ok_or(ask::Error::NoLaneWindow)?;
    let (iod_b, local_b) = map_one(end).ok_or(ask::Error::NoLaneWindow)?;
    if iod_a != iod_b {
        return Err(ask::Error::CrossesWindow);
    }
    for (core, first, last) in IOD_CORES {
        if (first..=last).contains(&local_a) {
            if !(first..=last).contains(&local_b) {
                return Err(ask::Error::CrossesWindow);
            }
            return Ok((
                iod_a,
                Mapping {
                    root_index: core.min(3),
                    wrapper: core,
                    start: local_a.min(local_b),
                    end: local_a.max(local_b),
                    reversed: start > end,
                },
            ));
        }
    }
    Err(ask::Error::NoLaneWindow)
}

/// Prepare a WH MPIO v2 [`Prepared`] session for a single local IOD (`iod` in
/// `0..=1`), filtering `ports` to those belonging to `iod` and pre-configuring
/// [`Encoding::LowByte`] + [`DOORBELL_V2`].
///
/// Returns `Ok(None)` if no ports in `ports` target `iod`.
pub fn prepare_iod(
    iod: u8,
    ports: &[Config<'_>],
) -> Result<Option<(Prepared, Vec<usize, MAX_PORTS>)>, Error> {
    if iod >= 2 {
        return Err(Error::Lanes(ask::Error::RootIndex(iod)));
    }
    let mut asks: Vec<Ask, MAX_PORTS> = Vec::new();
    let mut indices: Vec<usize, MAX_PORTS> = Vec::new();
    let mut ancillary = Ancillary::new(iod);
    let mut used = [false; 136];

    for (board_idx, cfg) in ports.iter().enumerate() {
        if cfg.port.hotplug != 0 {
            return Err(Error::Unsupported);
        }
        let (port_iod, mapping) =
            translate_socket_lane(cfg.port.start, cfg.port.end).map_err(Error::Lanes)?;
        if port_iod != iod {
            continue;
        }
        for lane in mapping.start..=mapping.end {
            if core::mem::replace(&mut used[usize::from(lane)], true) {
                return Err(Error::Overlap);
            }
        }
        let mut local_port = cfg.port;
        local_port.start = mapping.start;
        local_port.end = mapping.end;
        let anc_idx = ancillary
            .add(&local_port, cfg.phy, cfg.speed)
            .map_err(Error::Ancillary)?;
        let ask = ask::build_mapped(mapping, &local_port, anc_idx).map_err(Error::Lanes)?;
        asks.push(ask).map_err(|_| Error::Capacity)?;
        indices.push(board_idx).map_err(|_| Error::Capacity)?;
    }

    if asks.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        Prepared::from_parts(asks, ancillary, Encoding::LowByte, DOORBELL_V2),
        indices,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::rs::ask::{LinkSpeed, Port};

    #[test]
    fn splits_md25_cg0_lanes_across_iod0_and_iod1() {
        let board = [
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(0, 15)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(16, 31)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(32, 47)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(64, 79)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(80, 95)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(96, 111)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen4,
                    ..Port::pcie(130, 131)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen5,
                    ..Port::pcie(132, 133)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen5,
                    ..Port::pcie(134, 135)
                },
                phy: &[],
                speed: &[],
            },
        ];
        let (_, idx0) = prepare_iod(0, &board).unwrap().unwrap();
        let (_, idx1) = prepare_iod(1, &board).unwrap().unwrap();
        assert_eq!(idx0.as_slice(), &[0, 1, 2, 6]);
        assert_eq!(idx1.as_slice(), &[3, 4, 5, 7, 8]);

        // Bonus lane translation on IOD1 maps 132..=135 -> local 128..=131.
        let (iod, m) = translate_socket_lane(132, 133).unwrap();
        assert_eq!(
            (iod, m.wrapper, m.start, m.end, m.reversed),
            (1, 4, 128, 129, false)
        );
        let (iod, m) = translate_socket_lane(135, 134).unwrap();
        assert_eq!(
            (iod, m.wrapper, m.start, m.end, m.reversed),
            (1, 4, 130, 131, true)
        );

        // Cross-IOD and cross-core spans are rejected.
        assert_eq!(
            translate_socket_lane(60, 67),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(131, 132),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(12, 19),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(136, 137),
            Err(ask::Error::NoLaneWindow)
        );
    }
}
