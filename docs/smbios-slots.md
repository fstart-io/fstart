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
  subordinate-bus ranges. This requires a bridge dedicated to that physical
  slot: a shared conventional PCI bus can also contain other slots and
  onboard devices, so its first child does not identify a particular slot.

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

## Existing-board coverage audit

The source audit uses the boards present on the X201 branch and the local
coreboot tree at `1958b5d923a`. Only declarations with a verified physical
slot and bridge association are enabled; an enabled PCIe function or IRQ
routing entry alone is not proof of a connector.

* **X200 and X201:** ExpressCard is now declared on 00:1c.3 in both boards.
  Coreboot's `lenovo/x200/variants/x200/overridetree.cb` and
  `lenovo/x201/devicetree.cb` supply the same type/length/width/label.
  The X200 test also checks that its bridge is slot-implemented and
  hot-pluggable. Internal WWAN/WLAN/UWB connectors are separate work:
  connector format, physical wiring and USB-only modes must not be inferred
  just from the chipset's slot-implemented bits.
* **X60/X61 and T60/R60/Z61t:** physical PC Card/CardBus and internal
  wireless sockets need descriptions too. The X60/X61 Ricoh controller
  and the T60 TI controller sit behind 00:1e.0 in coreboot's devicetrees;
  their assigned buses must be resolved at runtime. Do not describe the
  whole 00:1e.0 bus as one occupied slot. T60-family variants and dock
  connections require their own connector mapping. The X60's previous
  comment claiming ExpressCard on 1c.2/1c.3 was not supported by its
  devicetree and has been corrected; enabled ports are not slot evidence.
* **Intel/legacy desktop boards:** D945GCLF, D510MO, D41S, GA-D510UD,
  GA-945GCM-S2L, P5GC-MX and MB899 need physical expansion-slot inventory.
  Conventional PCI slots require per-device/connector associations on a
  shared secondary bus, not `add_pci_slot` on the entire chipset bridge.
  D945GCLF's existing `D945GCLF_PIRQ` explicitly mixes the slot and onboard
  devices; MB899's coreboot `mptable.c` distinguishes an external PCIe
  slot, onboard Ethernet and multiple riser layouts. PCIe slots and
  mini-card sockets can use the dedicated-bridge path once the board's
  connector-to-port mapping and physical capabilities are verified.
* **Tyan S8030, EthanolX and MJ11:** physical PCIe/M.2/riser slots need
  Type 9 entries. Their AMD/SP3 SMBIOS flows currently have no slot
  contribution (`driver-amd/apob/smbios::build_into`), although they can
  reuse the shared writer. Associations must come from trained DXIO
  topology and the selected SKU/riser/strap/M.2 mode. Bifurcated engines
  are not automatically separate physical slots; preserve one physical
  description and account for peers rather than inventing extra slots.
* **Zork:** the board policy identifies NVMe and WLAN links, distinct from
  its onboard SD reader. Socket capabilities/variant wiring and a slot
  contribution in the shared AMD table flow remain to be added.
* **HiFive Unmatched:** PCIe/M.2 inventory is a candidate, but its current
  platform handoff is device-tree based and does not emit SMBIOS. Adding a
  declaration alone would have no consumer.
* **QEMU and the sunxi boards:** do not manufacture physical slot entries
  from enumerated virtual devices or unrelated SD/USB connectors. Virtual
  slots need an explicit machine/firmware contract; the sunxi flows in
  this tree do not currently emit SMBIOS slot inventory.

This audit is not a claim that the deferred boards have complete SMBIOS.
It identifies where board data or platform integration is still missing;
only the verified X200/X201 ExpressCard declarations are wired in here.
