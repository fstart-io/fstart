// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's links on the RS I/O die: the root port a board lane's link
//! is at, the links the PSP trained, and which of the board's soldered
//! devices ([`crate::links`]) have no link. RS is 1P: every lane is
//! socket 0's.
use fstart_driver_amd::dxio::rs::{ask, ports};
use fstart_pci::PciAddress;

pub use crate::links::{Behind, Soldered};

/// The devices of `required` whose port has no link: in `lanes` (each
/// board port's first lane, root port and whether its link is up) without
/// a link up, or on another socket than RS's one.
pub fn missing<'a>(
    required: &'a [Soldered],
    lanes: &'a [(u8, PciAddress, bool)],
) -> impl Iterator<Item = &'a Soldered> + 'a {
    required.iter().filter(|soldered| {
        soldered.socket != 0
            || !lanes
                .iter()
                .any(|&(lane, _, up)| lane == soldered.lane && up)
    })
}

/// The first lane of the link the PSP trained before MPIO on wrapper
/// `wrapper` of IOS `ios`, of the board's `psp_trained` (first and last
/// lanes): the live root port MPIO did not train there is its. `None` if
/// none of them is on that wrapper, or more than one is: then the lanes do
/// not say which port is whose.
pub fn psp_trained(present: &[u8], psp_trained: &[(u8, u8)], ios: u8, wrapper: u8) -> Option<u8> {
    let mut on_wrapper = psp_trained.iter().filter(|&&(first, last)| {
        ask::translate(present, first, last)
            .is_ok_and(|lanes| lanes.root_index == ios && lanes.wrapper == wrapper)
    });
    let &(first, _) = on_wrapper.next()?;
    on_wrapper.next().is_none().then_some(first)
}

/// The wrapper of logical bridge `bridge`.
pub fn wrapper(bridge: u8) -> u8 {
    ports::wrapper_port(bridge).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::tests::{BRIDGE, DISPLAY, ecam};

    /// A link that is down, or a port the board's lanes never got, is a
    /// missing device; one that is up is not, whatever is behind it.
    #[test]
    fn a_soldered_device_without_a_link_is_missing() {
        let required = [
            Soldered {
                socket: 0,
                lane: 132,
                behind: Behind::Device,
            },
            Soldered {
                socket: 0,
                lane: 134,
                behind: Behind::BridgedDisplay,
            },
            // RS has no socket 1.
            Soldered {
                socket: 1,
                lane: 135,
                behind: Behind::Device,
            },
        ];
        let port = PciAddress::new(0, 0, 1, 1);
        let lanes = [(132, port, true), (134, port, false), (135, port, true)];
        assert_eq!(missing_lanes(&required, &lanes), [134, 135]);
        assert_eq!(missing_lanes(&required, &lanes[..0]), [132, 134, 135]);
        assert!(missing_lanes(&required[..2], &[(132, port, true), (134, port, true)]).is_empty());
    }

    fn missing_lanes(required: &[Soldered], lanes: &[(u8, PciAddress, bool)]) -> Vec<u8> {
        missing(required, lanes).map(|s| s.lane).collect()
    }

    /// The Onyx with an add-in VGA card on a root port scanned before the
    /// BMC's PSP-trained one: the legacy VGA display is the one behind the
    /// port of the board's display lane, not the first one found.
    #[test]
    fn the_boards_display_port_wins_over_an_earlier_vga_card() {
        let (card_port, bmc_port) = (PciAddress::new(0, 0, 1, 0), PciAddress::new(0, 0, 7, 0));
        let (card, bmc) = (PciAddress::new(0, 1, 0, 0), PciAddress::new(0, 3, 0, 0));
        let (_ram, pci) = ecam(
            4,
            &[
                (card_port, 1, BRIDGE),
                (card, 0, DISPLAY),
                (bmc_port, 1, BRIDGE),
                (PciAddress::new(0, 2, 0, 0), 1, BRIDGE),
                (bmc, 0, DISPLAY),
            ],
        );
        assert_eq!(pci.legacy_vga_display(None), Some(card));
        // The BMC's port: the live one MPIO did not train on IOS 0's
        // wrapper 2, which the PSP-trained lane 134 is on.
        assert_eq!(psp_trained(&[0, 1, 2, 3], &[(134, 134)], 0, 2), Some(134));
        assert_eq!(pci.legacy_vga_display(Some(bmc_port)), Some(bmc));
        assert!(Behind::BridgedDisplay.found(&pci, bmc_port));
        assert!(!Behind::BridgedDisplay.found(&pci, card_port));
    }

    /// The Onyx: lane 134 (the BMC) is the PSP's, on IOS 0's wrapper 2 with
    /// the BCM5720's lane 135, which MPIO trains.
    #[test]
    fn a_psp_trained_link_is_the_live_port_on_its_wrapper() {
        let present = [0, 1, 2, 3];
        assert_eq!(psp_trained(&present, &[(134, 134)], 0, 2), Some(134));
        assert_eq!(psp_trained(&present, &[(134, 134)], 0, 1), None);
        assert_eq!(psp_trained(&present, &[(134, 134)], 2, 2), None);
        assert_eq!(psp_trained(&present, &[], 0, 2), None);
        // Two of them on one wrapper: which live port is whose is unknown.
        assert_eq!(psp_trained(&present, &[(134, 134), (135, 135)], 0, 2), None);
        assert_eq!(wrapper(18), 2);
    }
}
