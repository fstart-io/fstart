# Code Context

## Files Retrieved
1. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 121-158) - RCBA DxxIP/DxxIR register offsets.
2. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 271-288) - DxxIP pin values and DxxIR PIRQ values used by fstart.
3. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 541-559) - `IntelIch7Config`, including `pirq_routing`.
4. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 739-773) - fstart hard-coded ICH7 RCBA DxxIP/DxxIR programming.
5. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 955-972) - fstart LPC PIRQ[A-H]_ROUT programming.
6. `crates/fstart-driver-intel-ich7/src/lib.rs` (lines 2048-2367, 2408-2507) - fstart ICH7 ACPI PIRQ/link and `_PRT` generation.
7. `crates/fstart-driver-intel-ich8/src/lib.rs` (lines 368-434) - RCBA DxxIP/DxxIR/IOTR3 register offsets.
8. `crates/fstart-driver-intel-ich8/src/lib.rs` (lines 689-724) - `Ich8LateRcbaConfig` fields.
9. `crates/fstart-driver-intel-ich8/src/lib.rs` (lines 1788-1857) - fstart ICH8 PIRQ_ROUT, default DxxIP/DxxIR, and board late RCBA writes.
10. `crates/fstart-driver-intel-ich8/src/lib.rs` (lines 1900-1950) - init ordering: default intmap then optional `late_rcba`.
11. `crates/fstart-driver-intel-ich8/src/lib.rs` (lines 2030-2289) - fstart ICH8 static ACPI `_PRT`/link generation.
12. `boards/foxconn-d41s-uefi/src/lib.rs` (lines 90-210, 360-383) - D41S ICH7 config and ACPI platform constants.
13. `boards/lenovo-x61/board.ron` (lines 76-220, 345-384) - X61 ICH8 config, `late_rcba`, and ACPI platform constants.
14. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/lpc.c` (lines 46-116) - coreboot ICH7 LPC PIRQ[A-H]_ROUT programming semantics.
15. `/home/arthur/src/coreboot/src/southbridge/intel/i82801gx/i82801gx.h` (lines 185-194) - coreboot ICH7 DxxIP/DxxIR offsets.
16. `/home/arthur/src/coreboot/src/southbridge/intel/i82801ix/lpc.c` (lines 50-115) - coreboot ICH8/ICH9-style LPC PIRQ[A-H]_ROUT programming semantics.
17. `/home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.h` (lines 9-28) - common DxxIR offsets and ACPI PIRQ generator entry point.
18. `/home/arthur/src/coreboot/src/southbridge/intel/common/rcba_pirq.c` (lines 15-83) - coreboot ACPI generator reads live DxxIR and maps PIRQ to GSI 16+N.
19. `/home/arthur/src/coreboot/src/southbridge/intel/i82801hx/i82801hx.h` (lines 186-250) - DxxIP pin encoding, DxxIR PIRQ encoding, and per-function field offsets used by X61's ICH8-M path.
20. `/home/arthur/src/coreboot/src/southbridge/intel/i82801hx/early_rcba.c` (lines 1-75) - coreboot ICH8-M default DxxIP/DxxIR map.
21. `/home/arthur/src/coreboot/src/northbridge/intel/pineview/early_init.c` (lines 112-129) - coreboot Pineview/D41S RCBA DxxIP/DxxIR values.
22. `/home/arthur/src/coreboot/src/mainboard/foxconn/d41s/devicetree.cb` (lines 15-25) - D41S PIRQ[A-H]_ROUT board values.
23. `/home/arthur/src/coreboot/src/mainboard/foxconn/d41s/acpi/ich7_pci_irqs.asl` (lines 7-22) - D41S static downstream PCI bridge `_PRT`.
24. `/home/arthur/src/coreboot/src/mainboard/lenovo/x61/devicetree.cb` (lines 20-45) - X61 PIRQ[A-H]_ROUT board values.
25. `/home/arthur/src/coreboot/src/mainboard/lenovo/x61/early_init.c` (lines 48-69) - X61 board late RCBA DxxIP/DxxIR/IOTR3 values.
26. `/home/arthur/src/coreboot/src/mainboard/lenovo/x61/acpi/ich8_pci_irqs.asl` (lines 14-49) - X61 static downstream PCI bridge `_PRT`.

## Key Code

### Meaning of the registers

- `PIRQ[A-H]_ROUT` are LPC PCI config bytes at offsets `0x60..0x63` and `0x68..0x6b`. Low nibble selects legacy IRQ (`0x03` IRQ3, ..., `0x0b` IRQ11, `0x0e` IRQ14, `0x0f` IRQ15); bit 7 masks/unroutes the PIRQ. Coreboot documents this in both `i82801gx/lpc.c` and `i82801ix/lpc.c`.
- `DxxIP` are RCBA 32-bit device interrupt pin selectors. Each 4-bit field selects which PCI INTx pin an internal southbridge function asserts/reports: `0 = no interrupt`, `1 = INTA`, `2 = INTB`, `3 = INTC`, `4 = INTD`. X61/coreboot names these fields in `i82801hx.h`, e.g. D31 IDE/SATA/SMBus/Thermal and D28 PCIe root ports.
- `DxxIR` are RCBA 16-bit route registers. Nibble 0 maps INTA, nibble 1 INTB, nibble 2 INTC, nibble 3 INTD to PIRQ A-H (`0..7`). Coreboot's macro is:

```c
#define DIR_ROUTE(x, a, b, c, d) \
	RCBA16(x) = (((d) << 12) | ((c) << 8) | ((b) << 4) | ((a) << 0))
```

- In APIC mode, coreboot maps PIRQ A-H to IOAPIC GSIs `16..23` (`16 + pirq_idx`). It does this in `southbridge/intel/common/rcba_pirq.c` by reading the live `DxxIR` value.

### fstart values vs coreboot values

#### Foxconn D41S / Pineview + ICH7/NM10

`PIRQ[A-H]_ROUT`: correct. fstart board has all eight bytes `0x0b`; coreboot D41S devicetree also sets all eight to `0x0b`. fstart writes them little-endian as two dwords, which lands on the same byte offsets.

`RCBA DxxIP/DxxIR`: not correct versus coreboot Pineview/D41S values.

| Register | fstart programmed value | coreboot value | Result |
|---|---:|---:|---|
| D31IP | `0x00002200` | `0x00042210` | mismatch |
| D30IP | `0x00000001` | not explicitly in Pineview block | cannot prove match |
| D29IP | `0x00000001` | `0x10004321` | mismatch |
| D28IP | `0x00004321` | `0x00214321` | mismatch |
| D27IP | `0x00000001` | `0x00000001` | match |
| D31IR | `0x3210` | `0x0132` | mismatch |
| D30IR | `0x7654` | `0x0146` | mismatch |
| D29IR | `0x3210` | `0x0237` | mismatch |
| D28IR | `0x3210` | `0x3201` | mismatch |
| D27IR | `0x0000` | `0x0146` | mismatch |

The coreboot DxxIR values above decode from the overlapping Pineview `RCBA32(0x3140/0x3142/0x3144/0x3146/0x3148)` writes.

#### Lenovo X61 / GM965 + ICH8-M/HX

`PIRQ[A-H]_ROUT`: correct. fstart board has all eight bytes `0x0b`; coreboot X61 devicetree also sets all eight to `0x0b`.

`RCBA DxxIP/DxxIR`: mostly correct, but D26 is wrong because fstart's board `late_rcba` omits `d26ip` and `d26ir` even though coreboot X61 overrides both.

fstart first writes the ICH8-M default map, then applies optional `late_rcba`. The default map matches coreboot's `southbridge_configure_default_intmap()`. X61's board late override matches coreboot for D31/D29/D28/D27/D30 route fields and IOTR3, but leaves D26 at default.

| Register | fstart final value | coreboot X61 late value | Result |
|---|---:|---:|---|
| D31IP | `0x00001230` | `0x00001230` | match |
| D30IP | `0x00000001` default | default not overridden | match by default path |
| D29IP | `0x40004321` | `0x40004321` | match |
| D28IP | `0x00004321` | `0x00004321` | match |
| D27IP | `0x00000002` | `0x00000002` | match |
| D26IP | `0x10000021` default | `0x30000021` | mismatch |
| D25IP | `0x00000001` default | default not overridden | match by default path |
| D31IR | `0x1007` | `0x1007` | match |
| D30IR | `0x0076` | `0x0076` | match |
| D29IR | `0x3210` | `0x3210` | match |
| D28IR | `0x7654` | `0x7654` | match |
| D27IR | `0x0010` | `0x0010` | match |
| D26IR | `0x0003` default | `0x0654` | mismatch |
| D25IR | `0x0001` default | default not overridden | match by default path |
| IOTR3 | `0x000200f0000c0801` | `0x000200f0000c0801` | match |

D26 covers ICH8 USB group at device 26 (`USB4`, `USB5`, `EHC2`). fstart leaves those on default routes, while coreboot X61 moves them to the board-specific D26 routing.

## Architecture

- Board RON supplies `pirq_routing: [u8; 8]`; fstart ICH7/ICH8 writes those bytes to LPC PCI config `PIRQA_ROUT..PIRQH_ROUT`.
- ICH7 currently hard-codes all RCBA DxxIP/DxxIR values in the driver (`setup_interrupt_routing`) instead of taking a board-provided table. This makes D41S diverge from coreboot Pineview values.
- ICH8 has a better split: the driver writes a coreboot-equivalent default map, then applies board `late_rcba`. The X61 board data is incomplete: it needs D26 values too to match coreboot.
- Coreboot generates root-bus PIRQ ACPI from live RCBA DxxIR (`rcba_pirq.c`) so ACPI agrees with programmed hardware. But downstream bridge `_PRT` data is board/topology ASL: D41S `ich7_pci_irqs.asl`, X61 `ich8_pci_irqs.asl`.

## Findings

1. **Hardware LPC PIRQ[A-H]_ROUT programming is correct for both boards**: all eight links are routed to legacy IRQ11 (`0x0b`), matching coreboot devicetree values.
2. **Hardware RCBA DxxIP/DxxIR programming is not correct overall**:
   - D41S/ICH7: fstart RCBA routing is substantially different from coreboot Pineview values.
   - X61/ICH8: fstart matches most X61 late RCBA values, but misses `D26IP=0x30000021` and `D26IR=0x0654`.
3. **ACPI `_PRT` should not be blindly hand-coded independently of hardware routing.** The safest design is to have one board/topology source of truth that drives both RCBA programming and ACPI `_PRT` generation. Coreboot reads live RCBA for root-bus internal-device `_PRT` after programming, which guarantees consistency, but it still uses static board/topology data for devices behind bridges. For fstart, prefer generating from the board topology/static routing model and optionally validating against live RCBA after writes; do not use live RCBA as the only source if the hardware writes themselves may be wrong.

## Start Here

Start with `crates/fstart-driver-intel-ich7/src/lib.rs` lines 739-773 for D41S: it is hard-coded and disagrees with coreboot Pineview. Then check `boards/lenovo-x61/board.ron` lines 199-214: add/compare the missing D26 late RCBA values if changes are requested later.
