# GIGABYTE MD25-CG0

GIGABYTE's 1P SP7 server board for AMD Venice (Family 1Ah models 50h–5Fh,
WH I/O die, Qilian FCH), sixteen DDR5 channels, and an ASPEED AST2700 BMC
over eSPI/LPC.

> **Bring-up status**: Early bring-up. This board crate and the Venice / WH
> SoC support have been compiled and unit-tested offline only — they have
> **not** yet been booted on physical SP7 hardware. The host build is
> **build-only** (stage + SMM + FFS artifacts) and explicitly rejects
> `--blobs`/`--donor` flash assembly until Venice ISH A/B dual-origin flash
> packing exists.

## Verified board wiring (from `F01` donor analysis)

- **Socket / CPU**: 1P SP7 (`SocId::Venice`, up to 256 cores / 512 threads).
- **Console**: COM1 (`0x3F8`, IRQ 4, 115200 8N1) through the AST2700 BMC/SIO
  on eSPI/LPC (`0x2E`/`0x4E`).
- **BMC / IPMI**: AST2700 KCS at `0xCA2`/`0xCA3` (shared with OS IPMI driver
  for SMM RAS SEL reporting; suppressed in ACPI and SEL delivery if host KCS
  wide-I/O decode fails).
- **PCIe lane map (`$A26`)**: Six PCIe 6.0 x16 slots (`0..=15`, `16..=31`,
  `32..=47`, `64..=79`, `80..=95`, `96..=111`) plus bonus lanes `129`
  (AST2700 BMC VGA, PSP-trained), `130..=131` (onboard LAN1), `132..=133`
  (M.2), and `134..=135` (M.2).
- **DRAM / ROM layout**: Reset image (`0x62`) loaded by PSP to `0x7000_0000`,
  APOB V2 (`0x61`) placed by PSP at `0x7700_0000` (dynamically reserved from
  `apob.total_size()`), 16 MiB TSEG for up to 512 logical CPUs.

## Current bring-up scope & remaining hardware checkpoints

- **Implemented in runtime today**: APOB V2 (`9/9`) memory map, 1P dual-IOD
  DFv4.5 routing broadcast across live local IODs (`0x18`/`0x19`), WH NBIO
  IOHC TOM / IOAPIC / PSP BAR programming inside `below_pools.non_pci`, Qilian
  FCH takeover + PERST release + MPIO v2 (`DOORBELL_V2`) idle probe, BSP
  microcode, SMM in 16 MiB TSEG (reusing `rs::smm` and `psp::Variant::Gn`
  pending WH ABI validation), PCI ECAM scan, ACPI, and SMBIOS.
- **Not yet implemented / requires hardware validation**:
  1. **eSPI & AST2700 SIO**: `early_init` opens host-side LPC decodes only;
     it does not configure PSP/EFS eSPI peripheral-channel forwarding (no
     `[package.metadata.fstart.amd]` eSPI config yet) or run an AST2700 SIO
     logical-device activation sequence for COM1/KCS.
  2. **Multi-AP launch**: Runtime currently runs BSP-only (`threads() == 1`);
     APOB V2 `3/3` dynamic topology walking and `ccx::wh::enable_threads` /
     `apic_id` are unit-tested driver primitives not yet wired into `sp7::init`.
  3. **MPIO lane training & SMU v14**: Runtime probes MPIO v2 idle status only
     (no ASK/DELI training payloads) and does not yet issue `smu::v14` boot
     messages; `init_chipset` is a no-op.
  4. **IOMMU / IVRS, secondary FCH, SMM/PSP ABI**: IOMMU hardware BARs remain
     unenabled until IVRS is wired; secondary-IOD FCH helpers and WH-native
     PSP/SMM ABI checks await physical SP7 hardware.
  5. **Venice ISH A/B flash packer**: Dual-origin EFS -> `$PSP` -> ISH A/B ->
     `$PL2` -> `$BL2` image assembly is inspected/validated by `amdfw-format`
     and `fstart-amdfw`, but not yet packed by `image-build`.
