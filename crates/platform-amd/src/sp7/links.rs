// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's links on SP7: which of the board's soldered devices
//! ([`crate::links`], by socket and lane) have no link up.

pub use crate::links::{Behind, Soldered};
use fstart_pci::PciAddress;

/// Soldered devices whose socket's lane was not trained up in `lanes`
/// (`(socket, lane, port, up)`).
pub fn missing<'a>(
    required: &'a [Soldered],
    lanes: &'a [(u8, u8, PciAddress, bool)],
) -> impl Iterator<Item = &'a Soldered> {
    required.iter().filter(move |s| {
        !lanes
            .iter()
            .any(|&(socket, lane, _, up)| (socket, lane) == (s.socket, s.lane) && up)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_reports_down_or_absent_required_lanes() {
        let req = [
            Soldered {
                socket: 0,
                lane: 129,
                behind: Behind::BridgedDisplay,
            },
            Soldered {
                socket: 0,
                lane: 130,
                behind: Behind::Device,
            },
            // A 2P board's device on socket 1.
            Soldered {
                socket: 1,
                lane: 130,
                behind: Behind::Device,
            },
            Soldered {
                socket: 1,
                lane: 129,
                behind: Behind::Device,
            },
        ];
        let bdf = PciAddress::new(0, 0, 5, 1);
        let lanes = [
            (0, 129, bdf, true),
            (0, 130, bdf, false),
            (1, 130, bdf, true),
        ];
        let miss: heapless::Vec<(u8, u8), 4> =
            missing(&req, &lanes).map(|s| (s.socket, s.lane)).collect();
        assert_eq!(miss, [(0, 130), (1, 129)]);
    }
}
