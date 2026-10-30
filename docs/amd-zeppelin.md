# AMD Zeppelin (EPYC Embedded 3000) and Gigabyte MJ11

Zeppelin is Family 17h models 00h–0Fh in the SP4 package (two dies) and
SP4r2 (one die). The vendor PSP/ABL trains DRAM and reports it in the APOB.
Everything after that is native, with no AGESA. The shared flow is in
[amd-noncar.md](amd-noncar.md).

Board: `boards/gigabyte/mj11` (`gigabyte-mj11-ec1`, default; `gigabyte-mj11-ec0`
is the `ec0` variant with the x16 slot).

## Code

| Area | Path |
| --- | --- |
| SoC constants, identity, ECAM, APOB | `crates/driver-amd/src/soc/zeppelin/mod.rs` |
| Platform: `Soc` impl, `Config`, host policy | `crates/platform-amd/src/zeppelin/mod.rs` |
| Silicon init sequence | `crates/platform-amd/src/zeppelin/silicon.rs` |
| DRAM layout | `crates/platform-amd/src/zeppelin/layout.rs` |
| ACPI data | `crates/platform-amd/src/zeppelin/acpi.rs` |
| Host model of the dies, tests | `crates/platform-amd/src/zeppelin/tests.rs`, `crates/driver-amd/src/soc/zeppelin/test_pci.rs` |
| Board | `boards/gigabyte/mj11/src/` (`lib.rs` hooks and `Config`, `dxio.rs`, `i2c.rs`, `variant.rs` with `tables/ec{0,1}.rs`, `gpio.rs`, `bmc.rs`, `acpi.rs`, `smm.rs`) |

IP blocks in `crates/driver-amd/src` (Zeppelin versions, behind the `zeppelin` feature):

| IP | Module |
| --- | --- |
| Register transports | `smn.rs`, `df/` (`df::zeppelin`, `df::map`) |
| NBIO | `nbio/zeppelin/`, `nbio/iohc.rs`, `nbio/ioapic.rs`, `nbio/iommu.rs`, `nbio/pcie.rs` |
| SMU v9 | `smu/v9.rs`, `smu/threads.rs` (AP release) |
| DXIO | `dxio/zeppelin.rs` |
| CCX | `ccx/zeppelin/` (CPU init, MSR tables) |
| FCH | `fch/taishan/` (IRQ routing, SATA, USB, multi-FCH, `smm.rs`); shared `fch/smi.rs` |
| PSP, APOB | `psp/`, `apob/` |
| Vendor register sequences | `table.rs` |

Other: SMM TSEG/SMBASE in `crates/arch/src/x86/cpu/amd/smm.rs`; I2C master in
`crates/driver-i2c`; AST2400/AST2500 Super I/O in
`crates/driver-superio/src/aspeed_ast2400.rs`.

## SoC constants

| Constant | Value |
| --- | --- |
| `MAX_DIES` | 2 |
| ECAM | `0xf000_0000`, 128 buses |
| APOB (PSP writes it) | `0x3000_0000`, `0x2_0000` bytes |
| Non-PCI MMIO pool | 8 MiB per die |
| `TSEG_SIZE` | 8 MiB |
| `MAX_CPUS` | 32 |
| PSP variant | `psp::Variant::Zp` |
| BIOS directory | `Family17hModel00` |

## Silicon init

The silicon init is `zeppelin::Init::start` (`silicon::pre_mp_reset`),
`Init::train_dxio` (`silicon::pre_mp_dxio`) and `Dxio::finish`
(`silicon::pre_mp_finish`, then MP, then SMM). The board's `main` does its
work between them, where the vendor sequence runs the board PEIMs.

1. Taishan FCH pre-init (ACPI MMIO, LPC/SPI, SMBus, CF9, legacy I/O). The AC
   power-loss policy is set in `identify`, after the console.
2. DF MMIO/IO plan and programming (`df::map::plan`, `program`), per-die DF
   early init, scrub init from the APOB.
3. FCH reset tasks, SATA and multi-FCH reset. Then the board: MJ11
   (`fch_reset_done`) programs the VRs and redrivers over I2C3 and clears PM1.
4. Per die: NBIO early init, PSP BAR, IOHC DRAM map. BSP CCX init.
5. IOAPIC and IOMMU BARs per die.
6. DXIO training on every die. Then the board: MJ11 (`dxio_training_done`)
   applies the post-training SMN values and waits for the BMC.
7. Taishan USB, SMU v9 init with its DMA pages and policy, CAC weights, DF late
   init, VGA routing, NBIO pre-enumeration, FCH ACPI decode, env, SATA, xGbE.
8. MP (`noncar::mp::start`): the SMU releases the threads, each runs
   `ccx::zeppelin::cpu_init`. Then the SMU is told the APs are up and CCX is done.
9. SMM (`noncar::smm::install`) in TSEG, then `psp::notify_smm`.
10. PCI interrupt router from `Config::fch.irq_routes`.

`after_resources` sets the FCH ACPI ports; MJ11's `mainboard_final` runs
after `Stage::enumerate`. `ready_to_boot` does SATA late, multi-FCH late and
FCH late init; then the board's own work (MJ11 `ready_to_boot`, the SMBus
switch); then `end_of_post`, at the start of `Stage::boot`, disables DF
extended config. Then LockDFReg, BootDone and the payload (see the flow).

## Config

`zeppelin::Config`:

| Field | Meaning |
| --- | --- |
| `subsystem_id` | Subsystem ID of the FCH, NBIO and PCIe functions |
| `fch.power_failure` | AC power-loss policy |
| `fch.devices` | FCH devices per die (`xhci`, `sata`) |
| `fch.irq_routes` | PCI INTx router table (`0xc00`/`0xc01`) |
| `fch.smi_gevents` | GEVENT bits routed to the board's SMM handler |
| `dxio.descriptors` | DXIO engine list per die |

MJ11 values (`boards/gigabyte/mj11/src/lib.rs`, `dxio.rs`):

| Item | EC1 (default) | EC0 (`ec0` feature) |
| --- | --- | --- |
| Subsystem ID | `0x1000_1458` | same |
| SMI GEVENT | GEVENT11, the BMC diagnostic button | same |
| Slot PCIe | 3.1 x8 and 3.3 x8 (reversed lanes) | 3.1 x16 |
| Fixed PCIe | 1.2 SlimSAS, 1.3 M.2, 1.4 and 1.5 I210, 1.1 AST2500; SATA 0-3 | same |
| VR and redriver tables | `tables/ec1.rs` (redrivers present) | `tables/ec0.rs` (no redrivers) |

Interrupt routes: INTA-D to APIC 16-19 (PIC `NC`), SCI to 9, GPIO to 7.

## Flash image

Vendor image: `fbuild assemble --image-config <vendor flash dump>`. The vendor
prefix stays byte for byte. The FFS and the stage are placed as below.
Source: `layout.rs` (`VENDOR_PREFIX`, `RESET_FLASH_OFFSET`), `zeppelin/mod.rs`
(`PspOptions`).

| Flash offset | Use |
| --- | --- |
| `000000..294000` | Vendor prefix: EFS, PSP and BIOS directories, APCB, ABL, SMU firmware, microcode |
| `294000..db0000` | FFS: payload, `cpu_microcode.bin`, boot policy |
| `db0000..1000000` | Reset image, `0x250000` bytes, BIOS directory entry 0x62 |

The vendor BIOS directory must describe this reset image and the APOB within
the prefix. `image-build` checks it (`check_vendor`) before composing.

Two PSP options are set in the EFS (`PspOptions`):

- `compress_reset: true`: the reset image is zlib-compressed in the PSP's
  format and both directory levels mark entry 0x62 compressed. The PSP
  inflates the stage rather than copying the whole region.
- `spi: Some((7, 1))`: Fast Read at 33 MHz for the PSP's SPI reads.

Microcode: the vendor image's BIOS directory entries of type 0x66 go into the
FFS as `cpu_microcode.bin`.

## DRAM layout

From `zeppelin/layout.rs`:

| Use | DRAM range |
| --- | --- |
| Early region (reserved through the OS handoff) | `76000000..77000000` |
| Page tables (six pages) | `76000000..76006000` |
| Data and BSS | `76010000..76200000` |
| Heap | `76200000..76a00000` |
| BSP stack | `76d00000..76db0000` |
| Reset image (PSP copies it here) | `76db0000..77000000` |
| Reset vector | `76fffff0` (image end − 0x10) |

Also reserved in e820: the APOB at `30000000`, the AP trampoline page at
`0x8000`, and TSEG (8 MiB at the top of DRAM below 4 GiB).

## ACPI

From `zeppelin/acpi.rs` and the shared `crate::acpi`:

- `SocAcpi`: `taishan::ACPI_PORTS`, FADT flags, bridge routes (18 entries,
  matched by a unit test to the NBIO base table), FCH INTA-D GSIs 16-19.
- Board: `lpc_aml` for the Super I/O COM1 at `0x3f8` IRQ 4 and the IPMI KCS
  interface, the SPMI table, and no extra devices.
- The FADT's SMI command port is `0xb2`; ACPI enable/disable are `0xe1`/`0x1e`
  (`fch::smi`: the SMI handler sets or clears SCI on them).

## SMM and sleep

- Handler: `boards/gigabyte/mj11/src/smm.rs`: `Mj11Smm` implements the board
  part (`SmmBoard::gevent`) of `fch::taishan::smm::Handler`, which is built on
  `fch::smi`. Its GEVENT11 handler raises the vendor CPU diagnostic interrupt
  through SMN.
- FCH sources routed to SMI: APM port `0xb2`, `SLP_TYP`, the power button, and
  `Config::fch.smi_gevents`.
- Power button: S5 unless the OS owns ACPI.
- `SLP_TYP` 3 (S3), 4 (S4) and 5 (S5) enter sleep (`fch/taishan/smm.rs`):
  `PMxEE` is set for S3/S4, then `SLP_TYP` is written with `SLP_EN`. S0 and S1
  requests are ignored.
- The DSDT declares `_S0_`, `_S1_`, `_S4_`, `_S5_`. There is no `_S3_` and no
  S3 resume path.

## Build

```sh
cargo fbuild assemble --board gigabyte-mj11-ec1 --release \
  --image-config <vendor flash dump> \
  --payload linux --kernel bzImage --linux-bootargs "console=ttyS0,115200"

cargo fbuild assemble --board gigabyte-mj11-ec1 --release \
  --image-config <vendor flash dump> --payload uefi
```

`--payload` defaults to `halt`. `uefi` is CrabEFI (`uefi-ui` and `uefi-basic`
also exist). `gigabyte-mj11-ec0` builds the x16 variant.

## Not implemented

- S3 suspend and resume. The DSDT has no `_S3_`.
