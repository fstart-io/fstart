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

## Traits and phases

- `noncar::Soc` (`zeppelin::Zeppelin`, `picasso::Picasso`): constants
  (`NAME`, `LAYOUT`, `ECAM`, `ECAM_BUSES`, `PSP`, `APOB`, `BIOS_DIRECTORY`,
  `TSEG_SIZE`, `MAX_CPUS`), the `Config` and `State` types, and the methods
  `fch_pre_init`, `identify`, `after_resources`, `ready_to_boot`. The silicon
  init is the SoC module's own phases (`picasso::Init`,
  `zeppelin::silicon_init`), which take the `Stage`.
- `noncar::Board` (a board's `lib.rs`): `type Soc` and `CONFIG`, the facts
  host planning reads.

The flow is the board's: `src/bin/reset.rs` calls the phases and does its
own work between them. Vilboz:

```rust
fstart_platform_amd::entry!(main);

fn main() -> ! {
    let stage = Reset::<Picasso>::enter(&CONFIG).console(&CONSOLE, vilboz::console());
    let silicon = (|| {
        let init = Init::start(&stage)?;
        let gpio = init.gpio();
        vilboz::early_gpios(&gpio).map_err(|_| "board GPIOs")?;
        init.disable_rom_sharing();
        let cbi = vilboz::after_fch(&gpio, &mut init.clock()); // EC CBI, WLAN, RAM pads
        init.train_dxio()?.finish(cbi.native_power(), cbi.has_emmc())
    })();
    let mut silicon = silicon.unwrap_or_else(|why| stage.stop(why));
    let pci = stage.enumerate(&mut silicon);
    stage.ready_to_boot(&mut silicon, &pci);
    stage.boot(silicon, pci, vilboz::acpi())
}
```

Board work goes where the old hooks were: LPC decodes before
`Reset::console` (`Reset::lpc`), FCH-reset work between `Init::start` and
`train_dxio`, link-trained work between `train_dxio` and `finish`, and
post-enumeration or ready-to-boot work between `enumerate`,
`ready_to_boot` and `boot`. `Ctx` gives the dies, the clock, `fch()` (die 0)
and `gpio()`.

## Boot order

| Step | Owner |
| --- | --- |
| ECAM (`msr::enable_ecam`), ACPI MMIO, TSC frequency | flow |
| `Soc::fch_pre_init` | SoC |
| Board LPC work (`Reset::lpc`), then the console | board, flow |
| `Soc::identify` | SoC |
| TOP_MEM/TOM2, APOB memory map, TSEG, e820, early MTRR plan | flow (`memory.rs`) |
| FFS mount and verify, microcode blob | flow (`boot.rs`) |
| Silicon init, the SoC's phases | SoC: silicon, `noncar::mp::start`, `noncar::smm::install`, `noncar::psp::notify_smm`; board work between them |
| PCI root bridges, enumeration, allocation | flow (`pci.rs`) |
| Write-combining for prefetchable VGA BARs | flow (`mtrr::install_write_combining`) |
| `Soc::after_resources`, then board work | SoC, board |
| FCH and per-die NBIO IOAPICs (masked) | flow |
| ACPI tables | flow (`crate::acpi`), the board's `BoardAcpi` |
| `Soc::ready_to_boot` | SoC |
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
- LockDFReg runs after `ready_to_boot` and BootDone right before the payload.

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

- FADT (`FACP`), MADT, HPET, MCFG, and IVRS if die 0's IOMMU is present.
- DSDT: `\_S0_`, `\_S1_`, `\_S4_`, `\_S5_` only (no S3). One root bridge per die,
  bridge `_PRT`s, the ECAM reservation, and the board's `lpc_aml`, `devices`
  and `sb_aml`.
- Standalone tables from `BoardAcpi::tables` (e.g. the MJ11 SPMI).

Inputs: `SocAcpi` (FCH ACPI ports, FADT fields, NBIO bridge routes, FCH INTA-D
GSIs), the per-die `DieAcpi` from the flow, and `BoardAcpi`.

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
