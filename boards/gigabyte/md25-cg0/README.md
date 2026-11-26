# GIGABYTE MD25-CG0

GIGABYTE's 1P SP7 server board for AMD Venice (Family 1Ah, Zen 6 / Zen 6c,
two WH I/O dies, Qilian FCH), sixteen DDR5 channels, and an ASPEED AST2700
BMC on eSPI.

> **Status: never booted.** This board and fstart's SP7 support are
> compiled, host-tested and assembled with the vendor ROM's PSP firmware;
> nothing has run on SP7 silicon. "Host-tested" below means a unit test
> checks the claim against a model or a decoded donor table; it says
> nothing about the hardware.

> **Warning: Platform Secure Boot fusing.** GIGABYTE's F01 firmware ships
> the APCB token `PspPsbAutoFuse = 1`: on its first boot the PSP may fuse
> Platform Secure Boot to the key that signed the image it booted,
> GIGABYTE's, irreversibly. A CPU that has **ever** booted F01 (or any
> image with that token at 1) may already be fused to GIGABYTE's key and
> will then refuse to run an fstart image at all. fstart's image sets the
> token to 0 so that it never fuses a CPU itself, which does not undo a
> fuse already blown. Use a CPU that has never booted F01, or check its PSB
> fuse state (the PSP's PSB status) before flashing fstart.

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
  (`bmc_decodes`) and adds COM1 only if the APCB left it out. The LPC
  bridge's own decodes are dropped by GIGABYTE's ISA step (below): eSPI
  carries COM1, KCS and Port 80.
- **BMC / IPMI**: AST2700 KCS at `0xCA2`/`0xCA3`, shared with the OS's
  IPMI driver for SMM SEL reports. Without an eSPI range decoding it,
  there is no ACPI IPMI device and no SEL delivery at boot or in SMM
  (`Config::kcs_decoded`, passed to the SMM image).
- **TPM**: the optional TPM module is on the SPI TPM chip select
  (`Board::TPM = Some(TpmBus::Spi)`, routed as GIGABYTE's
  `ConfigureTpmDecoder` does). Without a module the probe finds nothing
  and measured boot is skipped. With one, the reset image is measured as
  read from the flash (both ISH slots must name the same image): ROM3 is
  at `0x2FFD_0000_0000` (`FchRom3BaseHigh`), inside Venice's 128 TiB
  identity map, so the image is read directly through ROM3 when it shows
  the flash linearly, else through the FCH's SMN flash window into the
  heap (`sp7::flash`).
- **PCIe (`$A26`, `wiring.rs`)**: six x16 ports (lanes `0..=15`,
  `16..=31`, `32..=47`, `64..=79`, `80..=95`, `96..=111`) and four bonus
  links: `129` (x1 Gen3, 5.2, device not identified), `130..=131` (LAN1
  x2, 5.1; off with the Setup LAN1 option), `132..=133` and `134..=135`
  (x2 Gen4, devices not identified). One PERST (FCH pad 26) on every FCH.
  No hotplug, no UBM. MPIO as the F01 PCDs set it: ancillary data on, no
  `AfterResetDelay`, the ABL-trained link at socket 0 lane `0x80`
  (`PcdEarlyBmcLinkTraining/Socket/LaneNum`).
- **BMC display: lane 128.** The AST2700's display is the link the ABL
  trains before x86, lane 128 on IOD1's bonus core (the core of `$A26`'s
  port at 129, but not a `$A26` port itself): the F01 APCB's GNB
  `EarlyPcieConfig` entry (`1705h:1003h`, read offline from `image.bin`,
  decoded with the published Turin layout, not confirmed for Venice) marks
  lane 128 present and lane 129 not present, F01's `PcdEarlyBmcLinkLaneNum`
  is `0x80`, and GIGABYTE's SMBIOS type 41 finds the onboard "Ast2700"
  video only where no `$A26` port covers lane 128. fstart keeps that link
  as trained (`Mpio::early_bmc`), makes it the legacy VGA display
  (`Config::display`), and lists it as `Mpio::expected`, not `required`: a
  missing link or display is a warning, the boot goes on. MPIO trains 129
  like any other port.
- **Vendor values (`PcdAmd*`, `PcdCfg*`) where the donors differ**:
  GIGABYTE's: PPTable platform limits 600 W / 600 W / 330 A / 310 A / 210 A
  / 430 A (the board's `POWER_LIMITS`), no CPU reset on sync flood, `PtmEn`
  0, `SriovEnDev0F1` 0, `AmdCxlOnAllPorts` 1, no `UpdatePccLimit`, the ISA
  step with a 33 MHz normal SPI read and LPC off, the WDAT at 1000 ms per
  count, GIGABYTE's egress poison masks.
- **AER: firmware-first**, the SoC default and GIGABYTE's: the SMM handler
  maps the ECAM at `0x2FFC_8000_0000` and the root bridges' windows above
  4 GiB in its page tables, and reaches a CXL register block only inside
  its root bridge's PCI windows, never SMRAM (`sp7::smm`). The ports'
  routing, SMU message 9, the HEST GHESs, the `AERR` device and `_L14`, and
  every `_OSC` (AER and DPC withheld from the OS) follow the one mechanism
  in effect (`sp7::ras::aer`), which falls back to OS-first only where the
  handler cannot reach the ECAM (not on this board).
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

## APCB

The board's `apcb-tokens` (Cargo.toml, docs/amd-apcb.md) name the ABL
tokens its code relies on (the ECAM window, `NbioIommu`, ROM3, BMC before
DRAM, xHCI 0..3), all at GIGABYTE's F01 values, and `PspPsbAutoFuse` at 0
(F01: 1). The packed APCBs are therefore F01's byte for byte except the
one that carries `PspPsbAutoFuse`, F01's `0x68/0`, which is sealed again
(unique instance + 1, checksum). Fusing tokens are only ever updated where
an APCB carries them: an image whose APCBs lacked `PspPsbAutoFuse` would
fail to build rather than get a guessed copy of it. See the warning at the
top: the token protects only CPUs that never booted F01.

## What is implemented

Compiled into the stage and the SMM image:

- APOB V2 (`9/9`, `9/0x13`, `0xC/0x22`, `3/3`, `8/8`, `1/0x11`, `7/6`).
- DFv4.5 on both IODs: the ABL's bus maps (both IODs must agree; missing
  routes are not made up), rewritten by the vendor's bus planner before
  NBIO only where they do not fit the ECAM (the board requests no counts,
  `Config::bus_request = None`); MMIO/I/O pairs, VGA, PIE asserts.
- WH NBIO per root bridge: IOMMU, IOAPIC and PSP BARs (read back over SMN),
  the SMU BAR for HSMP, the vendor tables, requester IDs, AER straps and
  performance tables through MPIO `0x26` (written, not read back), RAS,
  locks, MPIO clock gating at ready-to-boot.
- SMU v14 `SmuInitialization` on the socket's SMU (feature controls, the
  PPTable with GIGABYTE's limits, the message sequence), HSMP for the OS
  (`AMDI0097` per socket and the `HSMP` table).
- Qilian FCH: takeover, HW-ACPI, GIGABYTE's ISA step (LPC decodes off,
  33 MHz normal SPI read), A-Link, sync flood (no CPU reset); the FCH
  watchdog stopped, then counting at 1 s and published as a WDAT (neither
  donor sets its clock; fstart does so that the WDAT's period is true);
  IOD1's FCH relocated (read back) and initialized (HwAcpi, A-Link, SD off);
  a secondary FCH that fails is logged and the boot goes on. At
  ready-to-boot SMM sends the PSP `LockFCHReg` (SW SMI `0xD2`).
- WH MPIO v2 on both IODs: ASKs, binding by lane and kind, devfn remaps,
  straps, PERST release on both FCHs, training, bridges of untrained ports
  hidden.
- Zen 6 MP: every AP released through its IOD's SMN and checked in with
  its predicted APIC ID; x2APIC (the IOMMUs are up); microcode from the
  FFS unless the ASP loads it.
- SMM in a 16 MiB TSEG: the PSP's SmmInfo and P2C, machine-check
  thresholds and storm control, firmware-first PCIe/CXL error handling
  (above), SEL to the BMC, S5.
- Resets: the last reset logged; a firmware warm reset (the PSP's
  ResetRequired on this A/B flash) through Qilian's recipe and a loop
  guard in CMOS bank 1 (40h–43h) that counts on its record alone, not on
  PMxC0: at most 2 in a row for the PSP's ResetRequired or a CXL APCB
  update, 1 for any other reason, 3 in all, the record erased once the PSP
  took BootDone; a refused reset is logged (POST `2Eh`) and the boot goes
  on. The payload's cold reset is Qilian's full one (CF9h 0Eh); CrabEFI's
  run-time `ResetSystem` uses the same recipes through the ACPI-MMIO page,
  and its shutdown enters S5 through PM1_CNT. The PSP's recovery firmware
  halts the boot (POST `2Dh`).
- ACPI: FADT (direct ACPI mode: no SMI command port), FACS, MADT, MCFG,
  HPET, DSDT (per root bridge `_PRT` and `RPxx` from the live IOHC devfns,
  COM1, KCS when decoded, one shared `_CPC` when the SMU reports CPPC,
  `AERR`/`_L14`), SPCR, IVRS (also sent to the PSP, C2P `0x3F`, a refusal
  is logged), SRAT, SLIT, MSCT, HMAT (when there is more than one
  proximity domain, with the CXL windows' targets), HEST (machine checks and firmware-first AER),
  BERT (the ABL's `7/6` events), WDAT, HSMP, CEDT, WSMT. SMBIOS with the
  DDR5 DIMMs (Type 16/17/19).
- CXL as GIGABYTE ships it (every port, soft-reserved memory, the donors'
  input block and DXE PCDs, CEDT, `ACPI0016`/`ACPI0017`), except the
  runtime APCB update: `Config::cxl.apcb_update` is off until it has been
  seen working on this board. With it on, fstart sizes the CXL devices and,
  when the regions change, **writes the SPI flash**: the APCB `0x60/0` of
  the ISH slot the PSP says it booted (C2P `0x36`) only, every sector
  verified, the old bytes put back on any failure (then no reset), and a
  guarded warm reset after a verified write. Off, the APCB keeps the
  image's (empty) region array: the ABL maps no CXL window and devices stay
  CXL.io. A CXL uncorrectable error floods (`cxl.sync_flood`) and, with
  GIGABYTE's no-reset sync flood, leaves the machine hung, as GIGABYTE's
  own firmware does.
- Not on this board: PSP EINJ (`Config::einj = false`), Qilian USB
  (`Config::usb = None`: USB as the ABL set it up), hotplug, S3,
  `CDIT`/`CRAT`/`ERST`. A warm boot runs the same flow as a cold one (the
  ABL trains memory again).

## What is checked offline, and how

- Host unit tests (`cargo test -p fstart-platform-amd -p fstart-driver-amd
  -p fstart-board-gigabyte-md25-cg0`): the board's lane table (sorted, no
  overlaps, on their IODs; lane 128 and `$A26`'s 129 on IOD1 core 6 on
  B0), its config (the display on lane 128, expected not required,
  GIGABYTE's differing values, the ISA step, the WDAT period), the eSPI
  decode decisions on an FCH model; for the SoC, the DF route resolution
  and BAR placement (1P and 2P), NUMA domains, MADT/DSDT/SRAT/IVRS
  generation through `iasl` when installed, a 2P B0 system through PCI and
  ACPI, the warm-reset guard, the driver's sequences on SMN/MMIO models.
- `cargo fbuild check-acpi`: the board's baseline tables, built from its
  SP7 configuration (MCFG at `0x2FFC_8000_0000`, the firmware-first HEST,
  `AERR`/`_L14` and `_OSC`), disassembled and recompiled with `iasl`,
  methods run with `acpiexec`. WDAT, HSMP, CEDT and HMAT, which depend on
  what the stage finds, are not in it.
- `cargo fbuild assemble --donor`: the image packs into the F01 ROM's ISH
  A/B layout (`EFS +0x58`, per-slot PSP blobs, `$BL2` `0x61`/`0x62` in both
  slots, all four BIOS `0x66` microcode instances, F01's EFS `+0x59` byte
  `0x71`); `fstart-amdfw validate --profile venice --image-origin
  0x01000000` checks the result. `apcb_tokens_agree_with_the_config`
  checks the board's tokens against `config()`, and with
  `FSTART_APCB_DONORS` naming the F01 image an image-build test checks
  that they change exactly F01's `0x68/0` (docs/amd-apcb.md).
- Decoded donor data (`reports/research`): the F01 MPIO table (`$A26`),
  its APCB (eSPI, early PCIe), the vendor NBIO/CCX tables checked in with
  their provenance.

## Unverified (hardware needed)

- Everything at run time: PSP authentication, APOB contents, COM1/KCS
  reachability, AP release, SMM, the payload boot.
- The DF bus maps the ABL leaves on both IODs, the IOS fabric IDs when the
  DF names none.
- The secondary FCH sequence (read back only; no functional test), the
  lane-128 display link's state when x86 starts, the devices behind lanes
  129 and 132..135, the board's global MPIO words, `UPDATE_LINK`'s F01
  policy; the unused-tile clock gating (GIGABYTE has no such PCD; Dell's 1
  is used).
- Firmware-first AER on silicon (the IOHC RAS SMI, the GHES notifications);
  the runtime CXL APCB write (off); whether the PSP leaves PMxC0 as the
  reset set it (the guard does not depend on it).
- The WH `ReservedRasEinj` hole and EINJ ABI, the SMU v14 USB messages.
- The FCH watchdog's reset state and Linux binding both `wdat_wdt` and
  `sp5100_tco` to it (both donors publish the same WDAT: vendor parity).
- F01 zeroes EFS `+0x30..0x43` and `+0x4C..0x4F` instead of erasing them;
  that both encodings are equivalent is not established. Neither the image
  nor the validator establishes the ASP authentication mode or the CPU's
  PSB fuse state (see the warning above).
