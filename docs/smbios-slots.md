# SMBIOS physical slots (Type 9)

Physical slots are board wiring, not devices discovered during PCI enumeration.
Describe each upgradeable slot even when empty. A mini-PCIe socket, PCIe
expansion slot or ExpressCard socket uses the same mechanisms:

* `fstart_core::smbios::SystemSlot` holds the physical description, using
  SMBIOS encodings for type, maximum electrical width, length, ID and
  characteristics. It contains no chipset assumptions or PCI access.
* `fstart_acpi::smbios::SmbiosWriter::add_system_slot` serializes that
  description with caller-supplied usage and PCI address. It supports
  non-PCI slots too; use all-ones address fields when unknown.
* With `fstart-acpi`'s `pci` feature, `add_pci_slot` resolves a slot attached
  to a PCI bridge using `PciEcam::bridge_devices`. This uses the existing
  enumerator's cached secondary-bus topology, including segment/root
  identity. It does not rescan config space or infer parentage from broad
  subordinate-bus ranges.

## Board declaration

The Intel flow accepts const `PciSlot` descriptions through
`BoardFacts::with_smbios_slots`. Each pairs physical slot information with
its upstream bridge's enumeration coordinates. The X201 declares:

```rust
fstart_core::smbios::PciSlot {
    bridge_segment: 0,
    bridge_bus: 0,
    bridge_devfn: fstart_pci::devfn(0x1c, 3),
    slot: fstart_core::smbios::SystemSlot {
        designation: "ExpressCard Slot",
        slot_type: 0x07,       // PC Card, as in coreboot's board description
        data_bus_width: 0x08, // PCIe x1, NOT an 8-bit parallel bus
        length: 0x03,         // short
        id: 0,
        characteristics: [0, 0x03], // PME and hot-plug
    },
}
```

Other platforms can call `writer.add_pci_slot(&slot, bridge_address, pci)`
directly. For bridges whose bus numbers are assigned dynamically, supply
the runtime-resolved bridge address rather than hard-coding its bus.
There is no Lenovo, ExpressCard or Intel special case in the shared writer.

## Runtime meaning

* An enumerated bridge with a directly attached card: **In Use**. Type 9
  identifies the lowest-ordered function in the slot, not a descendant
  behind a bridge on an add-in card.
* An enumerated bridge without downstream functions: **Available**.
* No inventory, a disabled/missing bridge, or an unconfigured secondary bus:
  **Unknown**. Do not invent a PCI address or drop the physical slot.

The upstream bridge is only the board's association key. SMBIOS's emitted
PCI address identifies the card in the slot, as required by
[DSP0134 3.3.0 section 7.10.8](https://www.dmtf.org/sites/default/files/standards/documents/DSP0134_3.3.0.pdf).
The 17-byte formatted record is allowed by that specification (Table 44);
no newer peer-group fields or negotiated lane widths are fabricated.

Usage is the boot-time PCI view: it cannot detect a USB-only ExpressCard,
and does not update after OS hot-plug. Slot IDs and characteristics are
board facts, not inferred from the installed card or slot label. Keep IDs
consistent with ACPI `_SUN` where the slot type defines that correspondence.
