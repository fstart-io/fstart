// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board PCIe link expectations on SP7: verifying that soldered devices
//! (such as the AST2700 BMC VGA bridge behind lane 129) trained and
//! enumerated behind their root ports.

pub use crate::rs::links::{Behind, Soldered};
use fstart_driver_amd::dxio::wh as dxio_wh;
use fstart_pci::PciAddress;

/// Return the unique PSP-trained start lane mapped to `(iod, core)` under
/// `layout`, if exactly one entry in `psp_trained` targets that core.
pub fn psp_trained_on_core(
    psp_trained: &[(u8, u8)],
    layout: dxio_wh::Layout,
    iod: u8,
    core: u8,
) -> Option<u8> {
    let mut found = None;
    for &(start, end) in psp_trained {
        let Ok(map) = dxio_wh::translate_socket_lane(layout, start, end) else {
            continue;
        };
        if map.iod == iod && map.core == core {
            if found.is_some() {
                return None;
            }
            found = Some(start);
        }
    }
    found
}

/// Soldered devices whose lane was not trained up in `lanes`.
pub fn missing<'a>(
    required: &'a [Soldered],
    lanes: &'a [(u8, PciAddress, bool)],
) -> impl Iterator<Item = &'a Soldered> {
    required
        .iter()
        .filter(move |s| !lanes.iter().any(|&(lane, _, up)| lane == s.lane && up))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_unique_psp_trained_bonus_lane_per_iod_and_layout() {
        for layout in [dxio_wh::Layout::Sp7A0, dxio_wh::Layout::Sp7B0] {
            let (_, bonus_core, _, _) = layout.bonus_core();
            assert_eq!(bonus_core, 6);
            // MD25-CG0 AST2700 BMC VGA on lane 129 -> IOD0 bonus core 6.
            assert_eq!(
                psp_trained_on_core(&[(129, 129)], layout, 0, bonus_core),
                Some(129)
            );
            assert_eq!(
                psp_trained_on_core(&[(129, 129)], layout, 1, bonus_core),
                None
            );
            // Two PSP-trained lanes on the same core are ambiguous -> None.
            assert_eq!(
                psp_trained_on_core(&[(129, 129), (130, 131)], layout, 0, bonus_core),
                None
            );
            // Lane 132..=133 is on IOD1 bonus core 6.
            assert_eq!(
                psp_trained_on_core(&[(129, 129), (132, 133)], layout, 1, bonus_core),
                Some(132)
            );
        }
    }

    #[test]
    fn missing_reports_down_or_absent_required_lanes() {
        let req = [
            Soldered {
                lane: 129,
                behind: Behind::BridgedDisplay,
            },
            Soldered {
                lane: 130,
                behind: Behind::Device,
            },
        ];
        let bdf = PciAddress::new(0, 0, 5, 1);
        let lanes = [(129, bdf, true), (130, bdf, false)];
        let miss: heapless::Vec<u8, 4> = missing(&req, &lanes).map(|s| s.lane).collect();
        assert_eq!(miss, [130]);
    }
}
