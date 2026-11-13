//! Slots (type 9) from board wiring and the existing PCI enumeration, and
//! onboard devices (type 41) from the enumeration and the functions'
//! classes. No chipset-specific discovery is needed.

extern crate alloc;

use super::{Attached, Builder, OnboardDevice, OnboardType, SlotFunction, SlotUsage, SystemSlot};
use alloc::vec::Vec;
use fstart_pci::{PciAddress, PciEcam};

/// Slot Characteristics 2, bit 3: the slot can split its lanes into
/// several links (DSP0134 7.10.7).
const BIFURCATION: u8 = 1 << 3;

/// A function's place in segment/bus/device/function order.
fn order(a: PciAddress) -> (u16, u8, u8, u8) {
    (a.segment(), a.bus(), a.device(), a.function())
}

/// The function in a link: the lowest-ordered one behind its bridge
/// (DSP0134 7.10.8), none if the link is empty.
fn lowest_function(devices: impl Iterator<Item = PciAddress>) -> Option<PciAddress> {
    devices.min_by_key(|&a| order(a))
}

/// The SMBIOS onboard device type (DSP0134 7.42.2) and label of a PCI
/// class, as coreboot's devicetree walk reports them, and NVMe. Other
/// classes have no type of their own: a wireless controller's class is
/// that of any other network controller.
fn onboard_device(class: u16) -> Option<OnboardDevice<'static>> {
    let (kind, designation) = match class {
        0x0300 | 0x0302 => (OnboardType::Video, "Onboard Video"),
        0x0100 => (OnboardType::ScsiController, "Onboard SCSI"),
        0x0200 => (OnboardType::Ethernet, "Onboard LAN"),
        0x0401 | 0x0403 => (OnboardType::Sound, "Onboard Audio"),
        0x0101 => (OnboardType::PataController, "Onboard IDE"),
        0x0106 => (OnboardType::SataController, "Onboard SATA"),
        0x0107 => (OnboardType::SasController, "Onboard SAS"),
        0x0108 => (OnboardType::NvmeController, "Onboard NVMe"),
        _ => return None,
    };
    Some(OnboardDevice { designation, kind })
}

/// The first function behind `bridge` that is not itself a bridge, in
/// segment/bus/device/function order, looking behind the bridges on the
/// way: the device of a port, also behind a chip's own PCI bridge.
fn first_endpoint(pci: &PciEcam, bridge: PciAddress) -> Option<PciAddress> {
    let mut children: Vec<PciAddress> = pci.bridge_devices(bridge)?.collect();
    children.sort_unstable_by_key(|&a| order(a));
    children
        .into_iter()
        .find_map(|child| match pci.bridge_devices(child) {
            Some(_) => first_endpoint(pci, child),
            None => Some(child),
        })
}

impl<'a> Builder<'a> {
    /// Describe a physical slot, including when empty, from the root ports
    /// or dedicated bridges its links come from: `partitions` has one
    /// `(bridge, lanes)` for an unsplit slot and one per link for a slot
    /// split into several (bifurcated). Do not use a shared conventional
    /// PCI bridge with several slots or onboard devices behind it: its
    /// first child is not necessarily a card in the described slot.
    ///
    /// The function in a link is the lowest-ordered one behind its bridge,
    /// never a nested device behind a bridge on the card. The lowest of
    /// them is the base, the others its peers (DSP0134 7.10.8, 7.10.9). A
    /// bridge enumeration did not find has nothing behind it: a platform
    /// hides the ports of links that did not train. Without a PCI
    /// enumeration the usage is Unknown. A slot of several links also gets
    /// the bifurcation bit of Slot Characteristics 2. This boot-time PCI
    /// snapshot cannot detect USB-only ExpressCards or update SMBIOS after
    /// hot-plug.
    pub fn pci_slot(
        &mut self,
        slot: &SystemSlot<'a>,
        partitions: &[(PciAddress, u8)],
        pci: Option<&PciEcam>,
    ) -> &mut Self {
        let mut slot = *slot;
        if partitions.len() > 1 {
            slot.characteristics[1] |= BIFURCATION;
        }
        let first_width = partitions.first().map_or(0, |&(_, lanes)| lanes);
        let Some(pci) = pci else {
            return self.system_slot(
                &slot,
                SlotUsage::Unknown,
                SlotFunction::none(first_width),
                &[],
            );
        };
        let mut present: Vec<(PciAddress, u8)> = partitions
            .iter()
            .filter_map(|&(bridge, lanes)| {
                lowest_function(pci.bridge_devices(bridge)?).map(|address| (address, lanes))
            })
            .collect();
        present.sort_unstable_by_key(|&(a, _)| order(a));
        present.dedup_by_key(|&mut (a, _)| order(a));
        let mut functions = present.iter().map(|&(a, lanes)| SlotFunction {
            segment: a.segment(),
            bus: a.bus(),
            devfn: fstart_pci::devfn(a.device(), a.function()),
            width: lanes,
        });
        match functions.next() {
            Some(base) => {
                let peers: Vec<SlotFunction> = functions.collect();
                self.system_slot(&slot, SlotUsage::InUse, base, &peers)
            }
            None => self.system_slot(
                &slot,
                SlotUsage::Available,
                SlotFunction::none(first_width),
                &[],
            ),
        }
    }

    /// The slots and onboard devices a board attaches to its ports, each
    /// port `(bridge, lanes, attached)`: a type 9 per slot, whose ports are
    /// its links ([`Self::pci_slot`]), and a type 41 per onboard device
    /// ([`Self::pci_onboard_device`]), in the order of the ports.
    pub fn pci_ports(
        &mut self,
        ports: &[(PciAddress, u8, &Attached<'a>)],
        pci: Option<&PciEcam>,
    ) -> &mut Self {
        for (index, &(bridge, _, attached)) in ports.iter().enumerate() {
            match attached {
                Attached::Slot(slot) => {
                    if ports[..index]
                        .iter()
                        .any(|&(_, _, other)| other == attached)
                    {
                        continue;
                    }
                    let links: Vec<(PciAddress, u8)> = ports[index..]
                        .iter()
                        .filter(|&&(_, _, other)| other == attached)
                        .map(|&(bridge, lanes, _)| (bridge, lanes))
                        .collect();
                    self.pci_slot(slot, &links, pci);
                }
                Attached::Onboard(device) => {
                    self.pci_onboard_device(device, bridge, pci);
                }
            }
        }
        self
    }

    /// A device soldered to the board behind the port `bridge` (type 41):
    /// the first function there that is not a bridge, disabled and without
    /// an address if there is none.
    pub fn pci_onboard_device(
        &mut self,
        device: &OnboardDevice<'a>,
        bridge: PciAddress,
        pci: Option<&PciEcam>,
    ) -> &mut Self {
        match pci.and_then(|pci| first_endpoint(pci, bridge)) {
            Some(a) => self.onboard_device(
                device,
                true,
                a.segment(),
                a.bus(),
                fstart_pci::devfn(a.device(), a.function()),
            ),
            None => self.onboard_device(device, false, u16::MAX, u8::MAX, u8::MAX),
        }
    }

    /// A type 41 for each function of `pci` that `onboard` selects and whose
    /// class SMBIOS has an onboard device type for (video, storage, LAN,
    /// audio). Which functions are on the mainboard is the platform's call:
    /// the controllers built into the chipset or SoC.
    pub fn pci_onboard_devices(
        &mut self,
        pci: &PciEcam,
        onboard: impl Fn(PciAddress) -> bool,
    ) -> &mut Self {
        for address in pci.devices().filter(|address| onboard(*address)) {
            let Some(class) = pci
                .device(address)
                .map(|device| (device.read32(0x08) >> 16) as u16)
            else {
                continue;
            };
            let Some(device) = onboard_device(class) else {
                continue;
            };
            self.onboard_device(
                &device,
                true,
                address.segment(),
                address.bus(),
                fstart_pci::devfn(address.device(), address.function()),
            );
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multifunction_cards_use_the_lowest_ordered_function() {
        let base = PciAddress::new(2, 7, 0, 0);
        let functions = [
            PciAddress::new(2, 7, 0, 3),
            base,
            PciAddress::new(2, 7, 0, 1),
        ];
        assert_eq!(lowest_function(functions.into_iter()), Some(base));
        assert_eq!(lowest_function(core::iter::empty()), None);
    }

    /// The type 9 of the first structure after types 0 and 1.
    fn slot(bytes: &[u8], len: usize) -> &[u8] {
        let mut at = super::super::ENTRY_POINT_SIZE;
        while bytes[at] != 9 {
            at += usize::from(bytes[at + 1]);
            while bytes[at..at + 2] != [0, 0] {
                at += 1;
            }
            at += 2;
            assert!(at < len);
        }
        &bytes[at..len]
    }

    #[test]
    fn missing_inventory_still_emits_the_physical_slot() {
        let identity = crate::smbios::SmbiosIdentity::new("", "");
        let mut builder = Builder::new(&identity, 0, false);
        builder.pci_slot(
            &SystemSlot::new("Expansion slot"),
            &[(PciAddress::new(0, 0, 28, 3), 1)],
            None,
        );
        let mut bytes = [0u8; 512];
        let len = builder.assemble_into(&mut bytes, 0x1000).unwrap();
        let table = slot(&bytes, len);
        assert_eq!(&table[..2], &[9, 0x13]);
        assert_eq!(table[7], SlotUsage::Unknown as u8);
        assert_eq!(&table[13..19], &[0xff, 0xff, 0xff, 0xff, 1, 0]);
    }

    #[test]
    fn a_split_slot_gets_the_bifurcation_bit() {
        let identity = crate::smbios::SmbiosIdentity::new("", "");
        let mut builder = Builder::new(&identity, 0, false);
        builder.pci_slot(
            &SystemSlot::new("PCIE_1"),
            &[
                (PciAddress::new(0, 0, 3, 1), 8),
                (PciAddress::new(0, 0, 3, 3), 8),
            ],
            None,
        );
        let mut bytes = [0u8; 512];
        let len = builder.assemble_into(&mut bytes, 0x1000).unwrap();
        assert_eq!(slot(&bytes, len)[0x0c], BIFURCATION);
    }

    /// The structures of `bytes` after types 0 and 1, `(type, bytes)`.
    fn structures(bytes: &[u8], len: usize) -> Vec<(u8, &[u8])> {
        let mut at = super::super::ENTRY_POINT_SIZE;
        let mut out = Vec::new();
        while at < len {
            let start = at;
            at += usize::from(bytes[at + 1]);
            while bytes[at..at + 2] != [0, 0] {
                at += 1;
            }
            at += 2;
            out.push((bytes[start], &bytes[start..at]));
        }
        out
    }

    /// A slot's ports are its links: one type 9 for the two of PCIE_1, in
    /// the place of its first; an onboard device per port.
    #[test]
    fn ports_group_a_slots_links() {
        const SLOT: Attached<'static> = Attached::Slot(SystemSlot::new("PCIE_1"));
        const M2: Attached<'static> = Attached::Slot(SystemSlot::new("M.2"));
        const LAN: Attached<'static> = Attached::Onboard(OnboardDevice {
            designation: "LAN",
            kind: OnboardType::Ethernet,
        });
        let port = |device, function| PciAddress::new(0, 0, device, function);
        let identity = crate::smbios::SmbiosIdentity::new("", "");
        let mut builder = Builder::new(&identity, 0, false);
        builder.pci_ports(
            &[
                (port(3, 1), 8, &SLOT),
                (port(1, 3), 4, &M2),
                (port(1, 4), 2, &LAN),
                (port(3, 3), 8, &SLOT),
                (port(1, 5), 1, &LAN),
            ],
            None,
        );
        let mut bytes = [0u8; 1024];
        let len = builder.assemble_into(&mut bytes, 0x1000).unwrap();
        let slots: Vec<&[u8]> = structures(&bytes, len)
            .into_iter()
            .filter(|&(ty, _)| ty == 9)
            .map(|(_, s)| s)
            .collect();
        assert_eq!(slots.len(), 2);
        // PCIE_1: unknown usage without an enumeration, x8 base, the
        // bifurcation bit; then the M.2.
        assert_eq!((slots[0][7], slots[0][0x0c], slots[0][0x11]), (2, 8, 8));
        assert!(slots[0].windows(6).any(|w| w == b"PCIE_1"));
        assert_eq!((slots[1][0x0c], slots[1][0x11]), (0, 4));
        let lans: Vec<&[u8]> = structures(&bytes, len)
            .into_iter()
            .filter(|&(ty, _)| ty == 41)
            .map(|(_, s)| s)
            .collect();
        // Not found: disabled, instances 1 and 2, no address.
        assert_eq!(&lans[0][5..11], &[5, 1, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(&lans[1][5..7], &[5, 2]);
    }

    #[test]
    fn onboard_devices_cover_coreboot_classes() {
        let kind = |class| onboard_device(class).map(|d| (d.kind, d.designation));
        assert_eq!(kind(0x0200), Some((OnboardType::Ethernet, "Onboard LAN")));
        assert_eq!(kind(0x0403), Some((OnboardType::Sound, "Onboard Audio")));
        assert_eq!(
            kind(0x0108),
            Some((OnboardType::NvmeController, "Onboard NVMe"))
        );
        assert_eq!(onboard_device(0x0380), None);
        assert_eq!(onboard_device(0x0c03), None);
    }
}
