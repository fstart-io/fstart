# GIGABYTE MD25-CG0

GIGABYTE's 1P SP7 server board for AMD Venice (Family 1Ah, Zen 6 / Zen 6c,
two WH I/O dies, Qilian FCH), sixteen DDR5 channels, and an ASPEED AST2700
BMC on eSPI.

> **Status: never booted.** This board and fstart's SP7 support are
> compiled, host-tested and assembled with the vendor ROM's PSP firmware;
> nothing has run on SP7 silicon. "Host-tested" below means a unit test
> checks the claim against a model or a decoded donor table; it says
> nothing about the hardware.

## Donor references

The board data are from GIGABYTE's F01 ROM (`SPI_UPD/image.bin`). Silicon
sequence addresses in the WH drivers (`0x00f…`) are the Dell PR9XJ
`SilVenicePei`'s (VenicePI-SP7 1.0.0.0), not F01's 0.0.8.1 PEI module: the
package checks and MPIO barriers agree, internal struct offsets do not
(e.g. `CfgHotplugUMBSupport`: Dell `+0x2622`, F01 `+0x2232`).

## Board wiring

- **CPU**: 1P SP7 (`SocId::Venice`, `MAX_CPUS = 512`: 256 cores, SMT).
- **Console**: COM1 (`0x3F8`, IRQ 4, 115200 8N1), the AST2700's SUART1 on
  eSPI0. The ABL trains eSPI and programs its decodes and the SUART from
  the APCB (`EspiInit`: `0xCA2`+4, `0x2F8`+8, `0x3F8`+8, Port 80, 60h/64h;
  `EspiSioInit`: SUART1 at `0x3F8`/IRQ 4); the stage reads them
  (`bmc_decodes`) and adds COM1 only if the APCB left it out.
- **BMC / IPMI**: AST2700 KCS at `0xCA2`/`0xCA3`, shared with the OS's
  IPMI driver for SMM SEL reports. Without an eSPI range decoding it,
  there is no ACPI IPMI device and no SEL delivery at boot or in SMM
  (`Config::kcs_decoded`, passed to the SMM image).
- **TPM**: the optional TPM module is on the SPI TPM chip select
  (`Board::TPM = Some(TpmBus::Spi)`, routed as GIGABYTE's
  `ConfigureTpmDecoder` does). Without a module the probe finds nothing
  and measured boot is skipped. With one, the reset image is measured as
  read from the flash: ROM3 is at `0x2FFD_0000_0000`, above the 4 TiB
  identity map, so it is read through the FCH's SMN flash window (about
  3 MiB, kept in the heap).
- **PCIe (`$A26`, `wiring.rs`)**: six x16 ports (lanes `0..=15`,
  `16..=31`, `32..=47`, `64..=79`, `80..=95`, `96..=111`) and four bonus
  links: `129` (AST2700 display, x1 Gen3, 5.2), `130..=131` (LAN1 x2, 5.1;
  off with the Setup LAN1 option), `132..=133` and `134..=135` (x2 Gen4,
  devices not identified). One PERST (FCH pad 26) on every FCH. No hotplug,
  no UBM, no early-trained link (`early_bmc: None`).
- **BMC display**: `Mpio::expected`, not `required`: a missing lane-129
  link or display is a warning, the boot goes on. The F01 APCB's GNB
  `EarlyPcieConfig` entry (`1705h:1003h`, read offline from `image.bin`,
  decoded with the published Turin layout, not confirmed for Venice) marks
  lane 128 present and lane 129 not present, so nothing says the ABL
  trains lane 129 before x86; MPIO trains it like any port.
- **Memory / flash**: the PSP loads the reset image (`0x62`) at
  `0x7000_0000` and the APOB V2 (`0x61`) at `0x7700_0000`; the APOB's
  page-rounded size is reserved, a 16 MiB TSEG below 4 GiB. 64 MiB SPI flash
  with the dual-origin Venice ISH A/B layout (`EFS +0x58 = 0x01`, PSP bank
  at `0x0100_0000`, BIOS origin at `0`). If the FFS does not authenticate in
  ROM2, it is copied through SMN to DRAM right below the firmware and
  mounted there; that copy (5 MiB) stays reserved through the OS handoff.

## Building

Extract the raw 64 MiB flash image `SPI_UPD/image.bin` from the vendor
update archive (the ZIP exceeds `fbuild`'s 128 MiB input cap):

```bash
cargo fbuild check gigabyte-md25-cg0 --release
cargo fbuild check-acpi gigabyte-md25-cg0
cargo fbuild assemble --board gigabyte-md25-cg0 --release --donor /path/to/SPI_UPD/image.bin
```

## What is implemented

Compiled into the stage and the SMM image:

- APOB V2 (`9/9`, `9/0x13`, `0xC/0x22`, `3/3`, `8/8`, `1/0x11`, `7/6`).
- DFv4.5 on both IODs: the ABL's bus maps (both IODs must agree; missing
  routes are not made up), MMIO/I/O pairs, VGA, PIE asserts.
- WH NBIO per root bridge: IOMMU, IOAPIC and PSP BARs (read back over SMN),
  the vendor tables, requester IDs, AER straps and performance tables
  through MPIO `0x26` (written, not read back), RAS, locks.
- Qilian FCH: takeover, HW-ACPI, A-Link, sync flood; IOD1's FCH relocated
  (read back) and initialized (HwAcpi, A-Link, SD off); a secondary FCH
  that fails is logged and the boot goes on.
- WH MPIO v2 on both IODs: ASKs, binding by lane and kind, devfn remaps,
  straps, PERST release on both FCHs, training, bridges of untrained ports
  hidden.
- Zen 6 MP: every AP released through its IOD's SMN and checked in with
  its predicted APIC ID; x2APIC (the IOMMUs are up); microcode from the
  FFS unless the ASP loads it.
- SMM in a 16 MiB TSEG: the PSP's SmmInfo and P2C, machine-check
  thresholds and storm control, SEL to the BMC, S5.
- ACPI: FADT (direct ACPI mode: no SMI command port), FACS, MADT, MCFG,
  HPET, DSDT (per root bridge `_PRT` and `RPxx` from the live IOHC devfns,
  COM1, KCS when decoded, one shared `_CPC` when the SMU reports CPPC),
  SPCR, IVRS (also sent to the PSP, C2P `0x3F`, a refusal is logged), SRAT,
  SLIT, MSCT, HEST, BERT (the ABL's `7/6` events), WSMT. SMBIOS with the
  DDR5 DIMMs (Type 16/17/19).
- Not on this board: CXL (`Config::cxl = None`: no CEDT, no `ACPI0016`),
  PSP EINJ (`Config::einj = false`), Qilian USB (`Config::usb = None`),
  hotplug, S3, `HMAT`/`CDIT`/`CRAT`/`ERST`. Warm reset: the flow assumes a
  cold boot; a PSP that asks for a warm reset gets one, without a loop
  guard (POST `2Fh`).

## What is checked offline, and how

- Host unit tests (`cargo test -p fstart-platform-amd -p fstart-driver-amd
  -p fstart-board-gigabyte-md25-cg0`): the board's lane table (sorted, no
  overlaps, lane 129 maps to IOD1 core 6 on B0), its config (expected,
  not required, BMC display), the eSPI decode decisions on an FCH model;
  for the SoC, the DF route resolution and BAR placement (1P and 2P),
  NUMA domains, MADT/DSDT/SRAT/IVRS generation through `iasl` when
  installed, a 2P B0 system through PCI and ACPI, the driver's sequences
  on SMN/MMIO models.
- `cargo fbuild check-acpi`: the board's baseline tables disassembled and
  recompiled with `iasl`, methods run with `acpiexec`.
- `cargo fbuild assemble --donor`: the image packs into the F01 ROM's ISH
  A/B layout (`EFS +0x58`, per-slot PSP blobs, `$BL2` `0x61`/`0x62` in both
  slots, all four BIOS `0x66` microcode instances, F01's EFS `+0x59` byte
  `0x71`); `fstart-amdfw validate --profile venice --image-origin
  0x01000000` checks the result.
- Decoded donor data (`reports/research`): the F01 MPIO table (`$A26`),
  its APCB (eSPI, early PCIe), the vendor NBIO/CCX tables checked in with
  their provenance.

## Unverified (hardware needed)

- Everything at run time: PSP authentication, APOB contents, COM1/KCS
  reachability, AP release, SMM, the payload boot.
- The DF bus maps the ABL leaves on both IODs, the IOS fabric IDs when the
  DF names none.
- The secondary FCH sequence (read back only; no functional test), lane
  129 being the BMC display and its state before MPIO, the bonus ports'
  devices, the board's global MPIO words, `UPDATE_LINK`'s F01 policy.
- The WH `ReservedRasEinj` hole and EINJ ABI, the SMU v14 USB messages.
- F01 zeroes EFS `+0x30..0x43` and `+0x4C..0x4F` instead of erasing them;
  that both encodings are equivalent is not established. Neither the image
  nor the validator establishes the ASP authentication mode or PSB fuses.
- Warm reset (retained `ThreadEnable`, locked SMM and BARs).
