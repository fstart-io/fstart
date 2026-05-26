# Code Context

## Files Retrieved

1. `/home/arthur/src/coreboot/src/mainboard/foxconn/d41s/dsdt.asl` (lines 1-25) - establishes coreboot D41S ACPI namespace placement: `Scope (\_SB) { Device (PCI0) { ... } }`.
2. `/home/arthur/src/coreboot/src/northbridge/intel/pineview/acpi/pineview.asl` (lines 1-26) - included inside `Device (PCI0)`; pulls in host bridge, PDRC, PEGP, GFX.
3. `/home/arthur/src/coreboot/src/northbridge/intel/pineview/acpi/hostbridge.asl` (lines 1-8, 81-203) - root bridge IDs and `_CRS` are direct children of `PCI0`.
4. `/home/arthur/src/coreboot/src/northbridge/intel/pineview/acpi/peg.asl` (lines 1-25) - exact `PEGP._PRT` APIC/PIC structure.
5. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/acpi/ich7.asl` (lines 1-120, 121-184) - included inside `Device (PCI0)`; defines root-scope PM/GPIO/RCBA opregions then ICH7 PCI-function devices.
6. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/acpi/pci.asl` (lines 1-57) - exact `PCIB` device and `_PRT` include placement.
7. `/home/arthur/src/coreboot/src/mainboard/foxconn/d41s/acpi/ich7_pci_irqs.asl` (lines 1-23) - D41S board-specific static PCI bridge `_PRT` table.
8. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/acpi/lpc.asl` (lines 15-32) - `LPCB` config opregion exposes `PRTA`..`PRTH`; includes common `irqlinks.asl`.
9. `/home/arthur/src/coreboot/src/southbridge/intel/common/acpi/irqlinks.asl` (lines 1-80) - example coreboot PIRQ link device with `_DIS`, `_PRS`, `_CRS`, `_SRS`, `_STA` tied to `PRTA`.
10. `/home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c` (lines 1-96) - dynamic root-bus PIRQ generation: maps root-bus slot/pin through RCBA DxxIR to PIRQ/APIC GSI/source path.
11. `/home/arthur/src/coreboot/src/southbridge/intel/common/acpi_pirq_gen.c` (lines 1-72) - exact generated `_PRT` method structure and `PICM` APIC-vs-PIC split.
12. `/home/arthur/src/coreboot/src/southbridge/intel/common/acpi_pirq_gen.h` (lines 20-88) - `PIRQ_A`..`PIRQ_H`, `slot_pin_irq_map`, `pic_pirq_map` data model.
13. `/home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.h` (lines 1-31) - common DxxIR RCBA offsets.
14. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/lpc.c` (lines 67-111, 427-429) - writes LPC PIRQ routing bytes and calls dynamic SSDT PIRQ generator from LPC device.
15. `/home/arthur/src/coreboot/src/mainboard/foxconn/d41s/devicetree.cb` (lines 1-60) - D41S coreboot device topology and PIRQ byte values, all `0x0b`.
16. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 260-350, 530-585, 735-785, 900-980, 1956-2339, 2336-2570, 2570-2628) - fstart ICH7 hardware PIRQ setup and ACPI AML generation.
17. `crates/fstart-driver-intel-pineview/src/lib.rs` (lines 1343-1622) - fstart Pineview ACPI root bridge generation.
18. `boards/foxconn-d41s-uefi/board.ron` (lines 75-225, 364-388) - fstart D41S topology, ACPI names/parents, PIRQ bytes, MADT config.
19. `crates/fstart-codegen/src/stage_gen/board_gen/model.rs` (lines 212-296) - fstart ACPI path derivation from topology names and `acpi_parent`.
20. `crates/fstart-codegen/src/stage_gen/board_gen/caps_tables.rs` (lines 118-182) - fstart wraps driver AML under derived ACPI parent path.
21. `crates/fstart-types/src/device.rs` (lines 77-101) - `DeviceConfig.acpi_name` and `acpi_parent` semantics.
22. `crates/fstart-acpi/src/lib.rs` (lines 121-153) and `crates/fstart-acpi/src/platform/mod.rs` (lines 392-434) - root-fragment splitting and final wrapping in `\_SB_`.

## Key Code

### (1) Exact coreboot `_PRT` structure and scope placement

Coreboot D41S DSDT places both Pineview and ICH7 includes inside the PCI root bridge:

```asl
// /home/arthur/src/coreboot/src/mainboard/foxconn/d41s/dsdt.asl:19-24
Scope (\_SB) {
	Device (PCI0)
	{
		#include <northbridge/intel/pineview/acpi/pineview.asl>
		#include <southbridge/intel/i82801gx/acpi/ich7.asl>
	}
}
```

So all plain `Device (...)` objects emitted by Pineview/ICH7 static ASL are children of `\_SB.PCI0`.

#### Static PCI bridge `PCIB` secondary-bus `_PRT`

`PCIB` is the ICH7 PCI-to-PCI bridge at root-bus BDF `0:1e.0`. It is declared under `\_SB.PCI0` by inclusion, and its `_PRT` is a method containing board-specific D41S table:

```asl
// /home/arthur/src/coreboot/src/southbridge/intel/i82801gx/acpi/pci.asl:5-55
Device (PCIB)
{
	Name (_ADR, 0x001e0000)
	...
	Method (_PRT)
	{
		#include "acpi/ich7_pci_irqs.asl"
	}
}
```

D41S table included there:

```asl
// /home/arthur/src/coreboot/src/mainboard/foxconn/d41s/acpi/ich7_pci_irqs.asl:7-22
If (PICM) {
	Return (Package() {
		Package() { 0x0000ffff, 0, 0, 0x15},
		Package() { 0x0000ffff, 1, 0, 0x16},
		Package() { 0x0000ffff, 2, 0, 0x17},
		Package() { 0x0000ffff, 3, 0, 0x14},
    Package() { 0x0001ffff, 0, 0, 0x13},
	})
} Else {
	Return (Package() {
		Package() { 0x0000ffff, 0, \_SB.PCI0.LPCB.LNKF, 0},
		Package() { 0x0000ffff, 1, \_SB.PCI0.LPCB.LNKG, 0},
		Package() { 0x0000ffff, 2, \_SB.PCI0.LPCB.LNKH, 0},
		Package() { 0x0000ffff, 3, \_SB.PCI0.LPCB.LNKE, 0},
		Package() { 0x0001ffff, 0, \_SB.PCI0.LPCB.LNKD, 0},
	})
}
```

Meaning for devices behind `PCIB`: device 0 pin A/B/C/D route to APIC GSIs 21/22/23/20, and PIC mode uses `LNKF/LNKG/LNKH/LNKE`; device 1 pin A routes to GSI 19 or `LNKD`.

#### Static PEGP `_PRT`

Pineview includes `peg.asl` inside `\_SB.PCI0`, so `PEGP` is `\_SB.PCI0.PEGP`:

```asl
// /home/arthur/src/coreboot/src/northbridge/intel/pineview/acpi/peg.asl:3-25
Device (PEGP)
{
	Name (_ADR, 0x00010000)
	Method (_PRT)
	{
		If (PICM) {
			Return (Package() {
				Package() { 0x0000ffff, 0, 0, 16 },
				Package() { 0x0000ffff, 1, 0, 17 },
				Package() { 0x0000ffff, 2, 0, 18 },
				Package() { 0x0000ffff, 3, 0, 19 },
			})
		} Else {
			Return (Package() {
				Package() { 0x0000ffff, 0, \_SB.PCI0.LPCB.LNKA, 0 },
				Package() { 0x0000ffff, 1, \_SB.PCI0.LPCB.LNKB, 0 },
				Package() { 0x0000ffff, 2, \_SB.PCI0.LPCB.LNKC, 0 },
				Package() { 0x0000ffff, 3, \_SB.PCI0.LPCB.LNKD, 0 },
			})
		}
	}
}
```

#### Dynamic root-bus `_PRT`

Coreboot also generates an SSDT `_PRT` under the root bridge, not in the static DSDT. ICH7 LPC `acpi_fill_ssdt` calls:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/i82801gx/lpc.c:427-429
static void southbridge_fill_ssdt(const struct device *device)
{
	intel_acpi_gen_def_acpi_pirq(device);
}
```

That generator writes a scope and a method:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/acpi_pirq_gen.c:46-62
void intel_write_pci_PRT(const char *scope, const struct slot_pin_irq_map *pin_irq_map,
			 unsigned int map_count, const struct pic_pirq_map *pirq_map)
{
	acpigen_write_scope(scope);
	acpigen_write_method("_PRT", 0);
	acpigen_write_if();
	acpigen_emit_namestring("PICM");
	acpigen_emit_byte(RETURN_OP);
	acpigen_write_package(map_count);
	gen_apic_route(pin_irq_map, map_count);
	...
	acpigen_write_else();
	acpigen_emit_byte(RETURN_OP);
	acpigen_write_package(map_count);
	gen_pic_route(pin_irq_map, map_count, pirq_map);
	...
}
```

The call site hardcodes the root bridge path in coreboot:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c:93
intel_write_pci_PRT("\\_SB.PCI0", pin_irq_map, map_count, &pirq_map);
```

So dynamic root-bus routing is `Scope (\_SB.PCI0) { Method (_PRT) { If (PICM) { direct GSIs } Else { LPCB.LNKx source paths } } }`.

### (2) How `rcba_pirq` maps DxxIR/DxxIP to APIC GSIs and source paths

`rcba_pirq` maps root-bus devices like this:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c:16-44
static const u32 pirq_dir_route_reg[MAX_SLOT - MIN_SLOT + 1] = {
	D19IR, D20IR, D21IR, D22IR, D23IR, 0, D25IR,
	D26IR, D27IR, D28IR, D29IR, D30IR, D31IR,
};
...
if (slot < MIN_SLOT || slot > MAX_SLOT || slot == 24) {
	/* non-PCH devices use 1:1 mapping. */
	return (enum pirq)pci_pin;
}
reg = pirq_dir_route_reg[slot - MIN_SLOT];
pirq = (RCBA16(reg) >> shift) & 0x7;
return (enum pirq)(pirq + PIRQ_A);
```

Important details:

- Slots 19..31 use `D19IR`..`D31IR` RCBA 16-bit route registers; slot 24 is skipped because there is no `D24IR`.
- `shift = 4 * (pci_pin - PCI_INT_A)`, so each INTx pin selects one 4-bit nibble from DxxIR.
- DxxIR nibble value `0..7` maps to `PIRQ_A..PIRQ_H`.
- Non-PCH slots (e.g. host bridge/PEG/GFX slots outside 19..31) use 1:1 `INTA->PIRQA`, `INTB->PIRQB`, etc.
- `rcba_pirq` does **not** read DxxIP directly. DxxIP controls each internal device function's visible PCI Interrupt Pin. `rcba_pirq` reads that visible pin from PCI config:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c:68-79
for (struct device *dev = pcidev_on_root(0, 0); dev; dev = dev->sibling) {
	const u8 pci_dev = PCI_SLOT(dev->path.pci.devfn);
	const u8 int_pin = pci_read_config8(dev, PCI_INTERRUPT_PIN);
	...
	enum pirq pirq = map_pirq(dev, int_pin);
```

- APIC GSI is always `16 + pirq_idx(pirq)`, i.e. `PIRQA..PIRQH => GSI 16..23`:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c:82-86
pin_irq_map[map_count].slot = pci_dev;
pin_irq_map[map_count].pin = (enum pci_pin)int_pin;
pin_irq_map[map_count].pic_pirq = pirq;
/* PIRQs are mapped to GSIs starting at 16 */
pin_irq_map[map_count].apic_gsi = 16 + pirq_idx(pirq);
```

- PIC-mode source paths are generated from the actual LPC ACPI path reported by `acpi_device_path(lpc)`, then `LNK[A-H]` is appended:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c:50-66
const char *lpcb_path = acpi_device_path(lpc);
...
pirq_map.type = PIRQ_SOURCE_PATH;
for (i = 0; i < PIRQ_COUNT; i++)
	snprintf(pirq_map.source_path[i], sizeof(pirq_map.source_path[i]),
		 "%s.LNK%c", lpcb_path, 'A' + i);
```

- APIC entries have source `0` and source-index equal to the GSI; PIC entries have source `LPCB.LNKx` and source-index `0`:

```c
// /home/arthur/src/coreboot/src/southbridge/intel/common/acpi_pirq_gen.c:10-42
acpigen_write_PRT_GSI_entry(slot, pin - PCI_INT_A, apic_gsi);
...
acpigen_write_PRT_source_entry(slot, pin, source_path[pirq_index], 0);
```

### (3) Discrepancies in fstart

#### A. fstart emits root-bus `_PRT` as a hardcoded APIC-only package, not coreboot's `Method (_PRT)` with `PICM` split

fstart ICH7 emits:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2312-2339
pci0_aml.extend_from_slice(&acpi_dsl! {
    Name("_PRT", Package(
        Package(0x0002FFFFu32, 0u32, 0u32, 16u32),
        ...
        Package(0x001FFFFFu32, 3u32, 0u32, 19u32)
    ));
});
```

Differences from coreboot:

- Static `Name(_PRT, Package(...))` instead of `Method(_PRT) { If (PICM) ... Else ... }`.
- No PIC-mode source-path entries to `\_SB.PCI0.LPCB.LNKx`.
- Entries are hardcoded rather than generated from enabled root-bus devices plus live `PCI_INTERRUPT_PIN` and DxxIR mapping.
- Source path/root bridge name is assumed by placement, not explicitly derived from board topology in the driver.

A static direct-GSI `_PRT` can work in APIC mode, but it does not match coreboot and gives no fallback if the OS evaluates PIC-mode routing or expects link devices.

#### B. fstart `PCIB._PRT` matches only the APIC half of D41S coreboot

fstart emits the APIC direct GSI values that match coreboot's `If (PICM)` branch:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2477-2493
Device("PCIB") {
    Name("_ADR", 0x001E0000u32);
    Name("_PRT", Package(
        Package(0x0000FFFFu32, 0u32, 0u32, 0x15u32),
        Package(0x0000FFFFu32, 1u32, 0u32, 0x16u32),
        Package(0x0000FFFFu32, 2u32, 0u32, 0x17u32),
        Package(0x0000FFFFu32, 3u32, 0u32, 0x14u32),
        Package(0x0001FFFFu32, 0u32, 0u32, 0x13u32)
    ));
}
```

Missing versus coreboot D41S:

- No `Method (_PRT)`.
- No `If (PICM)`.
- No `Else` branch with `LNKF/LNKG/LNKH/LNKE/LNKD` source paths.

#### C. fstart `PEGP._PRT` matches only the APIC half of coreboot

fstart emits direct 16..19 only:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2512-2524
Device("PEGP") {
    Name("_ADR", 0x00010000u32);
    Name("_PRT", Package(
        Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
        Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
        Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
        Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
    ));
}
```

Coreboot has `If (PICM)` direct 16..19 and `Else` `LNKA..LNKD` under `\_SB.PCI0.LPCB` (`peg.asl:8-24`).

Also, ownership differs: in coreboot `PEGP` comes from Pineview (`pineview.asl` includes `peg.asl`), but fstart emits `PEGP` in the ICH7 driver.

#### D. Hardcoded `PCI0` paths remain inside fstart drivers despite codegen topology support

fstart codegen already derives ACPI parent paths from board topology:

```rust
// crates/fstart-codegen/src/stage_gen/board_gen/model.rs:247-267
pub(super) fn acpi_path(&self, idx: usize) -> Option<String> {
    ...
    let mut path = String::from("\\\\_SB_");
    for name in names {
        path.push('.');
        path.push_str(name);
    }
    Some(path)
}
```

and wraps driver AML under that parent path:

```rust
// crates/fstart-codegen/src/stage_gen/board_gen/caps_tables.rs:135-142
let acpi_parent_path = ctx.runtime_devices.acpi_parent_path(device.index);
...
dsdt_aml.extend(fstart_acpi::scoped_aml_with_root_fragments(#parent_path, &_device_aml));
```

But ICH7 still hardcodes an absolute PCI0 scope internally:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2054-2056
aml.extend_from_slice(&acpi_dsl! {
    Scope("\\_SB_.PCI0") {
        OperationRegion("RCRB", SystemMemory, 0xFED1C000u32, 0x4000u32);
```

and comments/docstrings assume PCI0 placement:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:1974-1977
/// The output is a flat sequence of AML objects.  The caller is
/// expected to embed them inside the appropriate PCI0 scope.
```

For current D41S this happens to line up because board topology names the northbridge `PCI0` and sets the southbridge `acpi_parent` to `northbridge`:

```ron
// boards/foxconn-d41s-uefi/board.ron:75-96
(
    name: "northbridge",
    acpi_name: "PCI0",
    driver: IntelPineview(( ... acpi_name: "MCHC" ... )),
),
(
    name: "southbridge",
    acpi_parent: "northbridge",
    driver: IntelIch7(( ...
```

But the design is brittle: if a board's root bridge ACPI name is not `PCI0`, ICH7's internal scope and link-source assumptions are wrong. The driver should not know that the parent root bridge is named `PCI0`.

#### E. Pineview ACPI naming is split/confusing: topology `PCI0`, driver config `MCHC`, and driver hardcoded `Device("PCI0")`

Board RON uses topology `acpi_name: "PCI0"` for the northbridge node and Pineview driver config `acpi_name: "MCHC"` for the host bridge PCI function:

```ron
// boards/foxconn-d41s-uefi/board.ron:75-91
name: "northbridge",
acpi_name: "PCI0",
driver: IntelPineview((
    ...
    acpi_name: "MCHC",
)),
```

fstart Pineview uses the driver config name for `MCHC`:

```rust
// crates/fstart-driver-intel-pineview/src/lib.rs:1370-1385
let name = config.acpi_name.as_deref().unwrap_or("MCHC");
...
Device(#{name}) {
    Name("_ADR", #{_adr});
```

but hardcodes the root bridge as `Device("PCI0")`:

```rust
// crates/fstart-driver-intel-pineview/src/lib.rs:1507-1511
aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
    Device("PCI0") {
        Name("_HID", EisaId("PNP0A08"));
        Name("_CID", EisaId("PNP0A03"));
        Name("_BBN", 0u32);
```

The codegen topology path and the driver's hardcoded root bridge name are two separate sources of truth.

#### F. fstart link devices are incomplete compared to coreboot's `irqlinks.asl`

Coreboot `LNKA` has `_DIS`, `_PRS`, dynamic `_CRS` reading `PRTA`, `_SRS` writing `PRTA`, and `_STA` based on `PRTA & 0x80` (`irqlinks.asl:3-57`). fstart emits fixed `_PRS`, fixed `_CRS` IRQ 11, and `_STA`, but no `_DIS`/dynamic `_CRS`/`_SRS`:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2223-2290
Device("LNKA") {
    Name("_HID", EisaId("PNP0C0F"));
    Name("_UID", 1u32);
    Name("_PRS", ResourceTemplate { Interrupt(... 3u32, ... 15u32); });
    Name("_CRS", ResourceTemplate { Interrupt(... 11u32); });
    Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
}
```

This is especially inconsistent with coreboot-style PIC `_PRT` source paths, because the source paths are only fully useful if the link devices report and can set the current IRQ via the LPC `PRTA`..`PRTH` fields.

#### G. fstart hardware setup is partly aligned, but static ACPI does not derive from it

fstart programs LPC PIRQ route bytes from board config, matching coreboot D41S all-`0x0b`:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:955-969
let pirq_low = u32::from_le_bytes([... self.config.pirq_routing[0..4] ...]);
let pirq_high = u32::from_le_bytes([... self.config.pirq_routing[4..8] ...]);
lpc.pirqa_rout.set(pirq_low);
lpc.pirqe_rout.set(pirq_high);
```

and board RON has the same bytes as coreboot devicetree:

```ron
// boards/foxconn-d41s-uefi/board.ron:96-100
pirq_routing: (0x0b, 0x0b, 0x0b, 0x0b,
               0x0b, 0x0b, 0x0b, 0x0b),
```

fstart also programs DxxIP/DxxIR:

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:739-773
rcba.regs().d31ip.set(... INTB << 12 ... INTB << 8 ...);
rcba.regs().d31ir.set(dir_route(PIRQA, PIRQB, PIRQC, PIRQD));
rcba.regs().d30ip.set(INTA);
rcba.regs().d30ir.set(dir_route(PIRQE, PIRQF, PIRQG, PIRQH));
...
rcba.regs().d28ir.set(dir_route(PIRQA, PIRQB, PIRQC, PIRQD));
rcba.regs().d27ir.set(dir_route(PIRQA, PIRQA, PIRQA, PIRQA));
```

But fstart's ACPI `_PRT` tables are static literals; they do not walk root-bus devices, read `PCI_INTERRUPT_PIN`, read DxxIR nibbles, or construct `LPCB.LNKx` source paths the way coreboot does.

#### H. fstart has a misleading comment that PIRQ routing is handled by MADT ISOs

```rust
// crates/fstart-driver-intel-ich7/src/lib.rs:2618-2621
/// The PM block addresses are carried in the x86 platform config
/// (`X86PlatformAcpi`), so the LPC bridge does not produce any
/// standalone tables. PIRQ routing is handled by the board RON
/// `isos` (Interrupt Source Overrides) in the MADT.
```

MADT interrupt-source overrides handle legacy ISA IRQ remaps/polarity (D41S board RON only has PIT IRQ0->GSI2 and SCI IRQ9 flags at `boards/foxconn-d41s-uefi/board.ron:370-381`). They do not describe PCI INTx routing for root-bus devices or downstream bridges. PCI routing must be in `_PRT` (with direct GSI or link-source entries).

## Architecture

Coreboot has three layers for this board:

1. Static namespace placement from D41S `dsdt.asl`: `\_SB.PCI0` is the root bridge; Pineview and ICH7 ASL are included inside it.
2. Static bridge/slot `_PRT` for known secondary buses:
   - `\_SB.PCI0.PEGP._PRT` from Pineview `peg.asl`.
   - `\_SB.PCI0.PCIB._PRT` from ICH7 `pci.asl` plus D41S `ich7_pci_irqs.asl`.
3. Dynamic root-bus `_PRT` in SSDT from `intel_acpi_gen_def_acpi_pirq(lpc)`, using:
   - root-bus PCI enumeration,
   - `PCI_INTERRUPT_PIN` (which DxxIP influences for internal devices),
   - DxxIR RCBA nibbles to get `PIRQ_A..H`,
   - `GSI = 16 + PIRQ index` in APIC mode,
   - `acpi_device_path(lpc).LNKx` source paths in PIC mode.

fstart currently mixes responsibilities:

- Pineview emits `MCHC`, `PDRC`, and hardcoded root bridge `PCI0`.
- ICH7 emits LPCB/link devices plus root-bus `_PRT`, root ports, PCIB, PEGP, GFX0, SATA/PATA/SBUS under whatever parent codegen wraps it in, but also embeds at least one absolute `\_SB_.PCI0` scope.
- Codegen already knows topology names and can wrap a driver's AML under the parent path, but driver AML still contains hardcoded root bridge assumptions.

## Recommended design

Use fstart topology/device names as the source of ACPI placement and routing, not hardcoded `PCI0` strings inside chipset drivers.

Concrete direction:

1. **Make ACPI generation contextual.** Extend/replace `AcpiDevice::dsdt_aml(&self, config)` with a contributor API that receives an ACPI context, e.g. resolved `self_path`, `parent_path`, `root_bridge_path`, `lpc_path`, and child/topology metadata. Codegen already computes most of this from `DeviceConfig.acpi_name` / `acpi_parent` (`model.rs:247-296`).

2. **Let Pineview own the PCI root bridge.** The board topology node `northbridge` with `acpi_name: "PCI0"` should determine the root bridge name. Pineview should emit `Device(<topology acpi_name>)` for the root bridge and put `MCHC`, `PDRC`, `PEGP`, and `GFX0` in the correct relative place. Avoid hardcoded `Device("PCI0")`.

3. **Let ICH7 own ICH7 children, but receive root/LPC paths.** ICH7 should emit `LPCB`, `HDEF`, USB, RPxx, `PCIB`, SATA/PATA/SBUS under the resolved root bridge path supplied by topology. It should refer to PIRQ links via resolved `lpc_path` (e.g. `\_SB_.PCI0.LPCB`), not by string literals.

4. **Generate `_PRT` from a PIRQ route model.** Add a small route generator equivalent to coreboot `rcba_pirq`:
   - enumerate enabled root-bus functions from topology and/or live PCI config in ramstage;
   - read/use each device's INTx pin;
   - for ICH7 slots 19..31 except 24, map pin through DxxIR nibble; outside that range use 1:1 INTx-to-PIRQ;
   - APIC branch emits direct `GSI = 16 + pirq_idx`;
   - PIC branch emits source path `<lpc_path>.LNK[A-H]`.

5. **Represent board-specific bridge routing by device names.** For D41S `PCIB`, store or derive a table attached to the `pcib`/`PCIB` topology node rather than hardcoding it in the ICH7 driver. Example concept: `bridge_routes: [(bridge: "pcib", device: 0, pin: A, pirq: F), ...]`, then emit `\_SB.<root>.<bridge>._PRT` using the bridge node's resolved ACPI path and the LPC link path. For D41S this should reproduce coreboot's `PCIB` table exactly.

6. **Emit coreboot-style `_PRT` methods.** Prefer `Method(_PRT) { If (PICM) { Return(direct GSI package) } Else { Return(link-source package) } }` for root bridge, `PCIB`, `PEGP`, and PCIe root ports. If fstart intentionally supports APIC-only, document that and remove/ignore link devices consistently; otherwise implement coreboot `irqlinks.asl` behavior (`_DIS`, dynamic `_CRS`, `_SRS`, `_STA`) using `PRTA`..`PRTH`.

7. **Remove the MADT ISO misconception.** Keep MADT ISOs for PIT/SCI only. Do not describe PCI INTx routing as `isos`; it belongs in ACPI `_PRT`.

## Start Here

Start in `crates/fstart-driver-intel-ich7/src/lib.rs` around lines 2312-2493. That is where fstart currently emits root-bus and `PCIB` `_PRT` packages and where the reported Linux `can't derive routing ... no GSI` issue is most directly addressed. Then check `crates/fstart-codegen/src/stage_gen/board_gen/model.rs` lines 247-296 and `caps_tables.rs` lines 135-142 to use the existing topology-derived ACPI parent paths instead of adding more hardcoded `PCI0` strings.
