//! Slots (type 9) from board wiring and the existing PCI enumeration, and
//! onboard devices (type 41) from the enumeration and the functions'
//! classes. No chipset-specific discovery is needed.

extern crate alloc;

use super::{Builder, SlotFunction, SlotUsage, SystemSlot};
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

/// SMBIOS onboard device type and label of a PCI class, as coreboot's
/// devicetree walk reports them.
fn onboard_device(class: u16) -> Option<(u8, &'static str)> {
    Some(match class {
        0x0300 | 0x0302 => (0x03, "Onboard Video"),
        0x0100 => (0x04, "Onboard SCSI"),
        0x0200 => (0x05, "Onboard LAN"),
        0x0401 | 0x0403 => (0x07, "Onboard Audio"),
        0x0101 => (0x08, "Onboard IDE"),
        0x0106 => (0x09, "Onboard SATA"),
        0x0107 => (0x0a, "Onboard SAS"),
        _ => return None,
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

    /// A type 41 for each function of `pci` that `onboard` selects and whose
    /// class SMBIOS has an onboard device type for (video, storage, LAN,
    /// audio). Which functions are on the mainboard is the platform's call.
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
            let Some((device_type, designation)) = onboard_device(class) else {
                continue;
            };
            self.onboard_device(
                designation,
                device_type,
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

    #[test]
    fn onboard_devices_cover_coreboot_classes() {
        assert_eq!(onboard_device(0x0200), Some((0x05, "Onboard LAN")));
        assert_eq!(onboard_device(0x0403), Some((0x07, "Onboard Audio")));
        assert_eq!(onboard_device(0x0380), None);
        assert_eq!(onboard_device(0x0c03), None);
    }
}
