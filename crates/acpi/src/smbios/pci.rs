//! Slots (type 9) from board wiring and the existing PCI enumeration, and
//! onboard devices (type 41) from the enumeration and the functions'
//! classes. No chipset-specific discovery is needed.

use super::{Builder, SlotUsage, SystemSlot};
use fstart_pci::{PciAddress, PciEcam};

/// Select the base function in the slot (DSP0134 7.10.8/7.10.9), never a
/// nested device behind a bridge on an add-in card. Do not confuse an absent
/// inventory with a known empty slot.
fn slot_state(
    devices: Option<impl Iterator<Item = PciAddress>>,
) -> (SlotUsage, Option<PciAddress>) {
    let Some(devices) = devices else {
        return (SlotUsage::Unknown, None);
    };
    match devices.min_by_key(|a| (a.segment(), a.bus(), a.device(), a.function())) {
        Some(address) => (SlotUsage::InUse, Some(address)),
        None => (SlotUsage::Available, None),
    }
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
    /// Describe a physical slot attached to a dedicated `bridge`, including
    /// when empty. Do not use a shared conventional PCI bridge containing
    /// multiple slots or onboard devices: its first child is not necessarily
    /// a card in the described slot.
    ///
    /// Board metadata names the upstream bridge; the emitted Type 9 address
    /// names the lowest-ordered function *in the slot*, as SMBIOS requires.
    /// Undiscovered/disabled bridges and absent PCI inventory yield Unknown
    /// usage; empty enumerated bridges yield Available. Neither gets a made-up
    /// endpoint address. This boot-time PCI snapshot cannot detect USB-only
    /// ExpressCards or update SMBIOS after hot-plug.
    pub fn pci_slot(
        &mut self,
        slot: &SystemSlot<'a>,
        bridge: PciAddress,
        pci: Option<&PciEcam>,
    ) -> &mut Self {
        let (usage, address) = slot_state(pci.and_then(|pci| pci.bridge_devices(bridge)));
        let (segment, bus, devfn) = address.map_or((u16::MAX, u8::MAX, u8::MAX), |a| {
            (
                a.segment(),
                a.bus(),
                fstart_pci::devfn(a.device(), a.function()),
            )
        });
        self.system_slot(slot, usage, segment, bus, devfn)
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
    fn empty_and_unknown_inventories_are_not_occupied_root_ports() {
        assert_eq!(
            slot_state(None::<core::iter::Empty<PciAddress>>),
            (SlotUsage::Unknown, None)
        );
        assert_eq!(
            slot_state(Some(core::iter::empty())),
            (SlotUsage::Available, None)
        );
    }

    #[test]
    fn multifunction_cards_use_the_lowest_ordered_function() {
        let base = PciAddress::new(2, 7, 0, 0);
        let functions = [
            PciAddress::new(2, 7, 0, 3),
            base,
            PciAddress::new(2, 7, 0, 1),
        ];
        assert_eq!(
            slot_state(Some(functions.into_iter())),
            (SlotUsage::InUse, Some(base))
        );
    }

    #[test]
    fn missing_inventory_still_emits_the_physical_slot() {
        let identity = crate::smbios::SmbiosIdentity::new("", "");
        let mut builder = Builder::new(&identity, 0, false);
        builder.pci_slot(
            &SystemSlot::new("Expansion slot"),
            PciAddress::new(0, 0, 28, 3),
            None,
        );
        let mut bytes = [0u8; 512];
        let len = builder.assemble_into(&mut bytes, 0x1000).unwrap();
        // Type 0 and 1, then the slot.
        let mut at = super::super::ENTRY_POINT_SIZE;
        while bytes[at] != 9 {
            at += usize::from(bytes[at + 1]);
            while bytes[at..at + 2] != [0, 0] {
                at += 1;
            }
            at += 2;
            assert!(at < len);
        }
        let table = &bytes[at..len];
        assert_eq!(&table[..2], &[9, 0x11]);
        assert_eq!(table[7], SlotUsage::Unknown as u8);
        assert_eq!(&table[13..17], &[0xff; 4]);
    }

    #[test]
    fn onboard_devices_cover_coreboot_classes() {
        assert_eq!(onboard_device(0x0200), Some((0x05, "Onboard LAN")));
        assert_eq!(onboard_device(0x0403), Some((0x07, "Onboard Audio")));
        assert_eq!(onboard_device(0x0380), None);
        assert_eq!(onboard_device(0x0c03), None);
    }
}
