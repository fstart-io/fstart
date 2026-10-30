# AMD noncar flow

Shared boot flow of the PSP-booted AMD SoCs (Family 17h). Implemented by
Zeppelin and Picasso; see the SoC pages for their details:

- [amd-zeppelin.md](amd-zeppelin.md): Zeppelin (EPYC Embedded 3000), Gigabyte MJ11
- [amd-picasso.md](amd-picasso.md): Picasso, Google Vilboz (`google-vilboz`)
- [amd-image-format.md](amd-image-format.md): flash image, PSP/BIOS directories, firmware blobs

## Model

- The PSP trains DRAM, writes the APOB, and copies the BIOS reset image
  (BIOS directory entry 0x62) to DRAM before it releases the BSP at the
  image's reset vector (image end − 0x10).
- There is no cache-as-RAM (`_has_car = 0` in the generated linker script).
  One stage runs from the reset vector to the payload.
- The reset entry is `crates/platform-amd/asm/psp_reset.S`. It enters long
  mode and continues at `_start_ram`. APs go to the SIPI trampoline at `0x8000`.

## Code

| Area | Path |
| --- | --- |
| Flow, traits, `Stop` | `crates/platform-amd/src/noncar/{mod,flow}.rs` |
| DRAM layout | `crates/platform-amd/src/noncar/layout.rs` |
| Memory map, e820 | `crates/platform-amd/src/noncar/memory.rs` |
| MP init | `crates/platform-amd/src/noncar/{mp,cpu}.rs` |
| SMM install, PSP SMM info and boot messages | `crates/platform-amd/src/noncar/{smm,psp}.rs` |
| PCI root bridges | `crates/platform-amd/src/noncar/pci.rs` |
| ACPI glue | `crates/platform-amd/src/noncar/acpi.rs`, `crates/platform-amd/src/acpi.rs` |
| Build plan (host side) | `crates/platform-amd/src/noncar/host.rs` |
| FFS mount, payload handoff | `crates/platform-amd/src/noncar/boot.rs` |

## Traits

- `noncar::Soc` (`zeppelin::Zeppelin`, `picasso::Picasso`): constants
  (`NAME`, `LAYOUT`, `ECAM`, `ECAM_BUSES`, `PSP`, `APOB`, `BIOS_DIRECTORY`,
  `TSEG_SIZE`, `MAX_CPUS`), the `Config` and `State` types, and the methods
  `fch_pre_init`, `identify`, `silicon_init` (TP1), `after_resources` (TP2),
  `ready_to_boot` (TP3).
- `noncar::Board` (a board's `lib.rs`): `type Soc`, `CONFIG`, `console()`, and
  optional hooks.

| Board hook | Called |
| --- | --- |
| `early_init(&Lpc)` | Before the console: LPC decodes, Super I/O |
| `console() -> Ns16550Config` | Console UART; the flow initializes it |
| `fch_reset_done(ctx, &mut Config)` | After FCH reset init (SMBus, I2C, GPIO work) |
| `dxio_training_done(ctx)` | After PCIe/SATA link training |
| `mainboard_final(ctx)` | After PCI enumeration (TP2) |
| `ready_to_boot(ctx)` | At TP3, before LockDFReg |
| `acpi() -> BoardAcpi` | When the ACPI tables are built |

`BoardCtx` gives the dies, the clock, `fch()` (die 0) and `gpio()`.

A board binary is one line:

```rust
fstart_platform_amd::stage_bin!(fstart_board_gigabyte_mj11::Board);
```

## Boot order

| Step | Owner |
| --- | --- |
| ECAM (`msr::enable_ecam`), ACPI MMIO, TSC frequency | flow |
| `Soc::fch_pre_init` | SoC |
| `Board::early_init`, then the console | board, flow |
| `Soc::identify` | SoC |
| TOP_MEM/TOM2, APOB memory map, TSEG, e820, early MTRR plan | flow (`memory.rs`) |
| FFS mount and verify, microcode blob | flow (`boot.rs`) |
| `Soc::silicon_init` (TP1) | SoC: silicon, `noncar::mp::start`, `noncar::smm::install`, `noncar::psp::notify_smm` |
| PCI root bridges, enumeration, allocation | flow (`pci.rs`) |
| Write-combining for prefetchable VGA BARs | flow (`mtrr::install_write_combining`) |
| `Soc::after_resources` (TP2), `Board::mainboard_final` | SoC, board |
| FCH and per-die NBIO IOAPICs (masked) | flow |
| ACPI tables | flow (`crate::acpi`), `Board::acpi` |
| `Soc::ready_to_boot` (TP3) | SoC |
| PSP LockDFReg (a failure halts), page tables, park APs, PSP BootDone | flow (`psp.rs`, `boot.rs`) |
| Payload | `fstart_stage::payload` |

`Stop::Failed` logs the step and halts the BSP.

## Layout

`noncar::layout::Layout` describes the DRAM and flash of one SoC:

| Field | Meaning |
| --- | --- |
| `early` | Firmware DRAM, reserved through the OS handoff |
| `page_tables` | Six pages: 4-GiB identity map for the reset entry |
| `data` | Data and BSS (ACPI tables are in BSS) |
| `heap`, `stack` | Firmware heap and BSP stack |
| `image` | PSP-loaded reset image; the last 8 KiB hold the 16-bit entry |
| `flash` | Boot flash, mapped below 4 GiB |
| `ffs` | FFS window in the flash, if any |

`Layout::check()` asserts the ordering at compile time. The host plan
(`noncar/host.rs`) generates from the same `Layout` the linker script, the
`.fstart.layout` descriptor and the ELF expectations.

## Memory map

- e820 (`memory::e820`): RAM below TOP_MEM, APOB holes, TSEG and everything
  above it, the early region, the APOB, the AP trampoline page at `0x8000`,
  and the IOMMU range above the APOB top are reserved.
- TSEG: `Soc::TSEG_SIZE` (8 MiB on both SoCs), aligned, at the low top of
  DRAM below 4 GiB (first hole below 4 GiB above 1 GiB, or TOP_MEM).

## SMM and PSP

- `noncar::smm::install` copies the board's SMM image (built from its `smm.rs`
  and embedded through `FSTART_SMM_IMAGE`) into TSEG, sets up every CPU's
  SMBASE and locks SMM. The handler is `fstart_platform_amd::smm::smm_bin!`.
- `noncar::psp::notify_smm` sends the PSP SmmInfo (mailbox command 0x02). Its
  P2C and C2P buffers are in the free part of TSEG. Without them the flow has
  no PSP session.
- LockDFReg runs at TP3 and BootDone right before the payload.

## MP

`noncar::mp::start` launches the APs through the SMU (`smu::threads::launch`).
Each CPU loads its microcode patch from the verified FFS copy (selected by
CPUID signature in `Fam17hCpu`) and runs the SoC's CCX init. Zeppelin's
`Soc::MAX_CPUS` is 32; Picasso's is 16.

## PCI root bridges

- `df::map::root_complex(die)` gives the buses and MMIO/IO windows the DF
  routes to each die.
- `noncar::pci::Root::new` turns them into a root bridge:
  - buses clamped to the ECAM;
  - I/O from `0x1000` to `0x10000`;
  - 32-bit MMIO below `0xfec00000`, minus the ECAM and the die's reserved
    MMIO pool;
  - 64-bit DF windows (from 1 TiB) aren't included.
- `PciEcam::from_providers` allocates below all roots at once. The ACPI host
  bridge of each die (`PCIn`, with `_BBN` and `_CRS`) describes the same root.

## ACPI

`crate::acpi::build` emits from data:

- FADT (`FACP`), MADT, HPET (minimum tick `0x37ee`), MCFG, WSMT, and IVRS if
  die 0's IOMMU is present; SRAT, MSCT and SLIT if the SoC reports its dies
  as NUMA nodes (`Soc::die_memory`: several dies, not interleaved).
- DSDT: `\_S0_`, `\_S1_`, `\_S4_`, `\_S5_` only (no S3). One root bridge per die
  (`_PXM` with NUMA) with an `NBIO` device whose `NAPE` clears
  `IOAPIC_FEATURES_ENABLE.FCH`, run by `\_PIC(1)` (`nbio::ioapic::acpi`,
  shared with SP3); bridge `_PRT`s, `_PRW` and the GPE handler of the SoC's
  wake bridges, the ECAM reservation, the board's `lpc_aml`, `devices` and
  `sb_aml`, and a processor device (`ACPI0007`) per CPU in the MADT's UID
  order with `_PCT`/`_PSS` (the enabled P-states), `_CST` (C1 MWAIT, and with
  CC6 C2, the C-state I/O base + 1) and a per-core `_PSD`
  (`fstart_driver_amd::ccx::power` reads them, `fstart_acpi::processor`
  encodes them as for Intel's SpeedStep).
- SRAT/MSCT/SLIT with `fstart_acpi::numa`, as SP3 and any platform.
- The FCH GPIO and I2C controllers with `fstart_driver_amd::fch::aml`, as SP3.
- Standalone tables from `BoardAcpi::tables` (e.g. the MJ11 SPMI and SPCR).

Inputs: `SocAcpi` (FCH ACPI ports, FADT fields, NBIO bridge routes, FCH INTA-D
GSIs, wake bridges), the per-die `DieAcpi` from the flow, the BSP's P-state
and C-state MSRs with the core's C2 latency (`ccx::power::c2_latency_us`:
400 µs on Zen, 18 µs on Zen 2/3), and `BoardAcpi`.

SMBIOS (`noncar/smbios.rs` over `fstart_driver_amd::apob::smbios`, shared
with SP3): the board's identity (`Board::smbios_identity`), one processor
package with its caches (`fstart_acpi::smbios::Package`, as Intel's), the DIMM
positions of the APOB's SMBIOS seeds with their SPD copies, the APOB's DRAM
ranges. Picasso's seeds aren't decoded yet (no Picasso APOB has shown their
layout), so Picasso lists no DIMMs. The UEFI payload gets the entry point; a
copy in the F segment (`publish_legacy_entry_point`) serves Linux without
UEFI.

GSIs: the FCH IOAPIC has GSI 0-23. Each die's NBIO IOAPIC has 32 more, from
`24 + 32 * die`. The `_PRT`s describe APIC mode only.

## Images and microcode

- `--image-config <vendor flash>` keeps the vendor prefix byte for byte and
  replaces the reset image. `--blobs DIR` or `--donor ROM` packs a native image
  from firmware files. See [amd-image-format.md](amd-image-format.md).
- Microcode goes into the FFS as `cpu_microcode.bin`, from BIOS directory
  type 0x66 of a vendor image or from the SoC's blob table.
- `image-build` reads the result back as the PSP does (`read_bios_directory`)
  and checks it against the layout before it writes anything.

## Build plan

`noncar::host::Plan::<Board>::emit` (feature `host`) resolves the plan from
the board's `Layout`, its `Config` and the SoC's `HostSoc::POLICY`:

| `Policy` field | Effect |
| --- | --- |
| `feature` | Stage feature: `bundle-zeppelin` or `bundle-picasso` |
| `payloads` | Whether payloads are allowed. Both SoCs: yes |
| `smm` | Adds the board's `smm` unit and binds `FSTART_SMM_IMAGE` |
| `release_only` | Stage is release-only. Picasso: yes; Zeppelin: no |
| `image_config` | `Vendor` or `Packed` (see above) |

Payloads (`--payload`, default `halt`): `halt`, `linux`, `uefi` (CrabEFI),
`uefi-ui`, `uefi-basic`. Any other value is rejected for these SoCs.
The `--linux-*` options require `--payload linux`.

## Cargo features

| Feature | Enables |
| --- | --- |
| `zeppelin`, `picasso` | The SoC module, with `fstart-driver-amd/<soc>` |
| `noncar` | The flow (without the bundle) |
| `bundle-zeppelin`, `bundle-picasso` | The reset stage |
| `smm` | `smm_bin!` and the FCH SMM support for the board's SMM unit |
| `host` | The build plan (`Plan::emit`), host side only |
| `payload-linux`, `payload-uefi`, `payload-uefi-ui`, `payload-uefi-basic` | Payloads |

`cargo test -p fstart-platform-amd` runs the unit tests for both SoCs (the
dev-dependencies enable both features).

## Commands

```sh
cargo fbuild check <board>            # type-check a board's resolved plan
cargo fbuild explain <board>          # print geometry and compiler selection
cargo fbuild assemble --board <board> [--release] [--payload <p>] \
  [--image-config <vendor.rom> | --blobs DIR | --donor <vendor.rom>]
cargo fbuild blobs --board <board> --from <vendor.rom> --out DIR
```

`assemble` writes `<board>.rom` (a whole flash image) and a `<board>.json`
beside it with the input hashes. A PSP fragment without an FFS is written as
`<board>-psp-fragment.bin` and is not a flash image.
