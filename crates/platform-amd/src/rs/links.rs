// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's links in PCI: the root port a board lane's link is at, and
//! what the board has soldered behind it. A soldered device is known by its
//! lane and by the topology behind its port, not by its PCI IDs: the lane
//! is the board's wiring, the vendor of the chip on it is not the SoC's
//! business.
use fstart_driver_amd::dxio::rs::{ask, ports};
use fstart_pci::{PciAddress, PciEcam};

/// A device a board has soldered behind one of its ports: the boot needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soldered {
    /// The first lane of the port.
    pub lane: u8,
    pub behind: Behind,
}

/// What the link of a port leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behind {
    /// A device: a function on the port's secondary bus (a NIC).
    Device,
    /// A VGA-compatible display (class 03_00_00h) behind a PCI bridge on
    /// the port's secondary bus: a BMC's display.
    BridgedDisplay,
}

impl Behind {
    /// Whether enumeration found this behind root port `port`.
    pub fn found(self, pci: &PciEcam, port: PciAddress) -> bool {
        let Some(mut children) = pci.bridge_devices(port) else {
            return false;
        };
        match self {
            Self::Device => children.next().is_some(),
            Self::BridgedDisplay => pci
                .legacy_vga_display(Some(port))
                .and_then(|display| pci.device_path(display))
                .is_some_and(|path| {
                    path.len() == 3 && path[0] == port && pci.bridge_devices(path[1]).is_some()
                }),
        }
    }
}

/// The devices of `required` whose port has no link: in `lanes` (each
/// board port's first lane, root port and whether its link is up) without
/// a link up.
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
    use fstart_pci::{PciRole::Window, PciRoot, PciRootInfo};

    /// A RAM ECAM of `buses` buses with `functions` (address, header type,
    /// class code) present and every other device absent.
    fn ecam(buses: u8, functions: &[(PciAddress, u8, u32)]) -> (Vec<u32>, PciEcam) {
        let mut ram = vec![0u32; usize::from(buses) << 18];
        let dword = |address: PciAddress, offset: usize| {
            (usize::from(address.bus()) << 18)
                + (usize::from(address.device()) << 13)
                + (usize::from(address.function()) << 10)
                + offset / 4
        };
        for bus in 0..buses {
            for device in 0..32 {
                ram[dword(PciAddress::new(0, bus, device, 0), 0)] = u32::MAX;
            }
        }
        for &(address, header, class) in functions {
            ram[dword(address, 0)] = 0x1234_1022;
            ram[dword(address, 8)] = class << 8;
            ram[dword(address, 0xc)] = u32::from(header) << 16;
        }
        let root = PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: ram.as_mut_ptr() as u64,
                bus_start: 0,
                bus_end: buses - 1,
            },
            [
                Window.mmio(0x8000_0000, 0x1000_0000),
                Window.io(0x1000, 0xf000),
            ],
        )
        .unwrap();
        let mut pci = PciEcam::new([root]).unwrap();
        pci.enumerate_and_allocate().unwrap();
        (ram, pci)
    }

    const BRIDGE: u32 = 0x06_04_00;
    const DISPLAY: u32 = 0x03_00_00;
    const NIC: u32 = 0x02_00_00;

    /// Three root ports: a BMC (a bridge with its display behind), a NIC,
    /// and one whose link is down.
    #[test]
    fn soldered_devices_are_known_by_the_topology_behind_their_port() {
        let (bmc, nic, down) = (
            PciAddress::new(0, 0, 1, 0),
            PciAddress::new(0, 0, 2, 0),
            PciAddress::new(0, 0, 3, 0),
        );
        let (_ram, pci) = ecam(
            5,
            &[
                (bmc, 1, BRIDGE),
                (nic, 1, BRIDGE),
                (down, 1, BRIDGE),
                // Buses depth first: the BMC's bridge 1, its display 2, the
                // NIC 3.
                (PciAddress::new(0, 1, 0, 0), 1, BRIDGE),
                (PciAddress::new(0, 2, 0, 0), 0, DISPLAY),
                (PciAddress::new(0, 3, 0, 0), 0, NIC),
            ],
        );
        assert!(Behind::BridgedDisplay.found(&pci, bmc));
        assert!(Behind::Device.found(&pci, bmc));
        assert!(Behind::Device.found(&pci, nic));
        // A NIC is no bridged display, whatever its vendor.
        assert!(!Behind::BridgedDisplay.found(&pci, nic));
        // Nothing behind a port without a link.
        assert!(!Behind::Device.found(&pci, down));
        assert!(!Behind::BridgedDisplay.found(&pci, down));
        // Not a port at all.
        assert!(!Behind::Device.found(&pci, PciAddress::new(0, 3, 0, 0)));
    }

    /// The display directly on the port's bus, without the BMC's bridge,
    /// is not the topology the board has.
    #[test]
    fn a_display_without_its_bridge_is_not_the_bmcs() {
        let port = PciAddress::new(0, 0, 1, 0);
        let (_ram, pci) = ecam(
            2,
            &[(port, 1, BRIDGE), (PciAddress::new(0, 1, 0, 0), 0, DISPLAY)],
        );
        assert!(Behind::Device.found(&pci, port));
        assert!(!Behind::BridgedDisplay.found(&pci, port));
    }

    /// A link that is down, or a port the board's lanes never got, is a
    /// missing device; one that is up is not, whatever is behind it.
    #[test]
    fn a_soldered_device_without_a_link_is_missing() {
        let required = [
            Soldered {
                lane: 132,
                behind: Behind::Device,
            },
            Soldered {
                lane: 134,
                behind: Behind::BridgedDisplay,
            },
        ];
        let port = PciAddress::new(0, 0, 1, 1);
        let lanes = [(132, port, true), (134, port, false), (135, port, true)];
        assert_eq!(missing_lanes(&required, &lanes), [134]);
        assert_eq!(missing_lanes(&required, &lanes[..0]), [132, 134]);
        assert!(missing_lanes(&required, &[(132, port, true), (134, port, true)]).is_empty());
    }

    fn missing_lanes(required: &[Soldered], lanes: &[(u8, PciAddress, bool)]) -> Vec<u8> {
        missing(required, lanes).map(|s| s.lane).collect()
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
