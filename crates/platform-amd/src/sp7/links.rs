// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 soldered-endpoint expectations and PSP-trained link matching (parity
//! with [`crate::rs::links`]).

use fstart_driver_amd::dxio::wh as dxio_wh;
use fstart_pci::{PciAddress, PciEcam};

/// What a board expects behind a soldered root port after PCI enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behind {
    /// Any PCI device behind the root port.
    Device,
    /// A PCIe-to-PCI/PCIe bridge behind the root port carrying a VGA display
    /// (e.g. ASPEED AST2700 BMC PCIe bridge + VGA endpoint).
    BridgedDisplay,
}

/// A soldered board device behind the port starting at `lane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soldered {
    pub lane: u8,
    pub behind: Behind,
}

impl Behind {
    /// Whether PCI enumeration found this topology behind `port`.
    pub fn found(self, pci: &PciEcam, port: PciAddress) -> bool {
        match self {
            Self::Device => pci
                .bridge_devices(port)
                .is_some_and(|mut d| d.next().is_some()),
            Self::BridgedDisplay => pci
                .legacy_vga_display(Some(port))
                .and_then(|display| pci.device_path(display))
                .is_some_and(|path| {
                    path.len() == 3 && path[0] == port && pci.bridge_devices(path[1]).is_some()
                }),
        }
    }
}

/// Devices in `required` whose port did not train up.
pub fn missing<'a>(
    required: &'a [Soldered],
    lanes: &'a [(u8, PciAddress, bool)],
) -> impl Iterator<Item = &'a Soldered> + 'a {
    required.iter().filter(|soldered| {
        !lanes
            .iter()
            .any(|&(lane, _, up)| lane == soldered.lane && up)
    })
}

/// If exactly one PSP-trained link in `psp_trained` maps to `(iod, core)`,
/// return its first socket-wide lane number.
pub fn psp_trained_on_core(psp_trained: &[(u8, u8)], iod: u8, core: u8) -> Option<u8> {
    let mut matching = psp_trained.iter().filter(|&&(first, last)| {
        dxio_wh::translate_socket_lane(first, last)
            .is_ok_and(|(port_iod, map)| port_iod == iod && map.wrapper == core)
    });
    let &(first, _) = matching.next()?;
    matching.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_unique_psp_trained_bmc_lane_on_iod0_bonus_core() {
        // AST2700 BMC VGA lane 129 maps to IOD 0, bonus core 4.
        assert_eq!(psp_trained_on_core(&[(129, 129)], 0, 4), Some(129));
        assert_eq!(psp_trained_on_core(&[(129, 129)], 1, 4), None);
        assert_eq!(psp_trained_on_core(&[(129, 129)], 0, 0), None);
        // Ambiguous if two PSP-trained links share one core.
        assert_eq!(psp_trained_on_core(&[(129, 129), (130, 130)], 0, 4), None);
    }

    #[test]
    fn reports_missing_required_links() {
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
        let port = PciAddress::new(0, 0, 1, 1);
        let lanes = [(129, port, true), (130, port, false)];
        let miss: alloc::vec::Vec<u8> = missing(&req, &lanes).map(|s| s.lane).collect();
        assert_eq!(miss, [130]);
    }
}
