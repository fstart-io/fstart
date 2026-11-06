//! PCIe-slot SMBIOS from board wiring and the existing PCI enumeration.
//! No config-space accesses or chipset-specific discovery are needed.

use super::{SlotUsage, SmbiosWriter, SystemSlot};
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

impl SmbiosWriter<'_> {
    /// Describe a physical slot attached to `bridge`, including when empty.
    ///
    /// Board metadata names the upstream bridge; the emitted Type 9 address
    /// names the lowest-ordered function *in the slot*, as SMBIOS requires.
    /// Undiscovered/disabled bridges and absent PCI inventory yield Unknown
    /// usage; empty enumerated bridges yield Available. Neither gets a made-up
    /// endpoint address. This boot-time PCI snapshot cannot detect USB-only
    /// ExpressCards or update SMBIOS after hot-plug.
    pub fn add_pci_slot(
        &mut self,
        slot: &SystemSlot<'_>,
        bridge: PciAddress,
        pci: Option<&PciEcam>,
    ) {
        let (usage, address) = slot_state(pci.and_then(|pci| pci.bridge_devices(bridge)));
        let (segment, bus, devfn) = address.map_or((u16::MAX, u8::MAX, u8::MAX), |a| {
            (
                a.segment(),
                a.bus(),
                fstart_pci::devfn(a.device(), a.function()),
            )
        });
        self.add_system_slot(slot, usage, segment, bus, devfn);
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
        let mut bytes = [0u8; 128];
        let len = super::super::assemble_into(&mut bytes, 0x1000, |w| {
            w.add_pci_slot(
                &SystemSlot::new("Expansion slot"),
                PciAddress::new(0, 0, 28, 3),
                None,
            );
            w.add_end_of_table();
        })
        .unwrap();
        let table = &bytes[super::super::ENTRY_POINT_SIZE..len];
        assert_eq!(&table[..2], &[9, 0x11]);
        assert_eq!(table[7], SlotUsage::Unknown as u8);
        assert_eq!(&table[13..17], &[0xff; 4]);
    }
}
