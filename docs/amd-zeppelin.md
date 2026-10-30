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
| TP1 sequence | `crates/platform-amd/src/zeppelin/silicon.rs` |
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

## Silicon init (TP1)

`Soc::silicon_init` runs `silicon::pre_mp`, then MP, then SMM. The board
hooks are called where the vendor sequence has them.

1. Taishan FCH pre-init (ACPI MMIO, LPC/SPI, SMBus, CF9, legacy I/O). The AC
   power-loss policy is set in `identify`, after the console.
2. DF MMIO/IO plan and programming (`df::map::plan`, `program`), per-die DF
   early init, scrub init from the APOB.
3. FCH reset tasks, SATA and multi-FCH reset. Board `fch_reset_done`: MJ11
   programs the VRs and redrivers over I2C3 and clears PM1.
4. Per die: NBIO early init, PSP BAR, IOHC DRAM map. BSP CCX init.
5. IOAPIC and IOMMU BARs per die.
6. DXIO training on every die, then board `dxio_training_done`. MJ11 applies
   the post-training SMN values and waits for the BMC.
7. Taishan USB, SMU v9 init with its DMA pages and policy, CAC weights, DF late
   init, VGA routing, NBIO pre-enumeration, FCH ACPI decode, env, SATA, xGbE.
8. MP (`noncar::mp::start`): the SMU releases the threads, each runs
   `ccx::zeppelin::cpu_init`. Then the SMU is told the APs are up and CCX is done.
9. SMM (`noncar::smm::install`) in TSEG, then `psp::notify_smm`.
10. PCI interrupt router from `Config::fch.irq_routes`.

TP2 (`after_resources`) sets the FCH ACPI ports. TP3 (`ready_to_boot`) does
SATA late, multi-FCH late, FCH late init, board `ready_to_boot`, and disables
DF extended config. Then LockDFReg, BootDone and the payload (see the flow).

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

fstart packs the whole flash from the AMD firmware files: the platform crate's
`[package.metadata.fstart.amd.zeppelin]` table (PSP and BIOS directory
entries, their levels, microcode) and the board's `[package.metadata.fstart.amd]`
(its APCBs). `--donor <vendor flash image>` extracts the files from a vendor
image; `--blobs <directory>` takes them from a blob directory
(`cargo fbuild blobs`). See [amd-image-format.md](amd-image-format.md).
Source: `layout.rs` (`PSP_PARTITION_END`, `RESET_FLASH_OFFSET`),
`zeppelin/mod.rs` (`PspOptions`), `image-build/src/amd/levels.rs`.

| Flash offset | Use |
| --- | --- |
| `020000..294000` | PSP partition: EFS, `$PSP` + `$PL2`, `$BHD` + `$BL2`, signed files, APCBs, NV areas |
| `294000..db0000` | FFS: payload, `cpu_microcode.bin`, boot policy |
| `db0000..1000000` | Reset image, `0x250000` bytes in DRAM, BIOS directory entry 0x62 |

The partition has the vendor images' shape: two levels of each directory,
physical addresses (`0xff000000` + offset), a first-generation EFS with the
BIOS directory at +0x18. Level 1 is the recovery set (AMD public key, the
recovery boot loader), level 2 adds the secure debug, HW IP and SEV files and
the soft fuse chain (1). A file in both levels is stored once. The packer
allocates the areas the PSP and the ABL write, erased: PSP 0x22 and 0x38,
BIOS 0x63 (APOB NV, 128 KiB) and 0x67.

The APOB NV copy starts erased, so the ABL trains the memory on every boot;
fstart doesn't save it back.

The EFS and the reset image entry carry the boot speed settings:

- `compress_reset: true` (`PspOptions`): the reset image is zlib-compressed in
  the PSP's format and both levels mark entry 0x62 compressed. The PSP
  inflates the stage rather than copying the whole region.
- `spi-read-mode = 7`, `spi-speed = 1` (SoC table): Fast Read at 33 MHz for
  the PSP's SPI reads.

Microcode: the SoC table's patches (equivalence IDs 0x8000, 0x8010, 0x8011,
0x8012) go into the FFS as `cpu_microcode.bin`, not into the BIOS directory.

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
  interface, the SPMI and SPCR (COM1, the BMC's SOL) tables, and `_PTS`/`_WAK`
  reporting the sleep state to the BMC (SW SMI `0xc0` + Sx).
- The FCH GPIO controller (`AMDI0030`) on the IRQ the board routes it to.
- The xHCI bridge 7.1 wakes through GPE `0x0b` (`_PRW`, `\_GPE._L0B`).
- Processors: P-states from PStateDef (EPYC 3151: 2.7, 1.9, 1.2 GHz), C1
  MWAIT and C2 (CC6, 400 µs) through I/O `0x814`.
- SP4 (two dies): SRAT, MSCT and SLIT (10/16) from the DF DRAM map
  (`df::zeppelin::die_memory`), unless the ABL interleaves the dies.
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
- At `SLP_TYP` with `SLP_EN` still clear, Zeppelin posts SMU message 0x13
  (SleepEntry), as the vendor does; if the system still runs after 100 ms,
  `SLP_EN` enters the state.
- MJ11: SW SMI `0xc0` + Sx sends IPMI Set ACPI Power State over KCS
  (`SmmBoard::apmc`), only if the OS's KCS interface is idle, within 20 ms.
- The DSDT declares `_S0_`, `_S1_`, `_S4_`, `_S5_`. There is no `_S3_` and no
  S3 resume path.

## CPU

- Every thread writes the name string from the SMU (message 0x28) to
  C001_0030-35 (`ccx::brand`).
- `CcxConfig::memory_encryption` sets SYSCFG.SMEE on every thread after the
  MTRRs, as the vendor does. Off by default, because with it Linux
  initializes SEV, whose PSP firmware keeps its state in flash through the
  SMM handler's P2C SPI services (sp4r2 PSP_ZP §3.5), which fstart doesn't
  implement: the SEV status command times out after 5 s and the `ccp`
  driver disables the PSP.

## SMBIOS

From the APOB: the DIMM positions of the type-17 seeds (ZP packs them like
the type-20 seeds: socket [1:0], channel [4:2], DIMM [6:5], populated [7]),
each populated one from its SPD copy; one package, socket code none (BGA).
MJ11: Gigabyte MJ11, version the variant, rack mount chassis.

## MJ11 riser

`fch_reset_done` reads the PCIE_1 riser ID from the EEPROM at `0x57` behind the
PCA9545 at `0x74` (channel 2) on I2C3, else from GPIO 152/151/4/3, and maps it
to a bifurcation with the vendor's table. The EC0 applies it; the EC1's F09
template splits the slot on 3.1/3.3 and its mode table isn't decoded, so the
EC1 logs the riser and keeps its default.

## Build

```sh
cargo fbuild assemble --board gigabyte-mj11-ec1 --release \
  --donor MJ11-EC1_F09_Web/flashabl.rom \
  --payload linux --kernel bzImage --linux-bootargs "console=ttyS0,115200"

cargo fbuild assemble --board gigabyte-mj11-ec1 --release \
  --donor MJ11-EC1_F09_Web/flashabl.rom --payload uefi
```

Use the board's own vendor release as the donor (F09 for EC1, F02 for EC0):
the APCBs come from it.

`--payload` defaults to `halt`. `uefi` is CrabEFI (`uefi-ui` and `uefi-basic`
also exist). `gigabyte-mj11-ec0` builds the x16 variant.

## Not implemented

- S3 suspend and resume. The DSDT has no `_S3_`.
- Saving the APOB NV copy: the ABL trains the memory on every boot.
- The PSP's P2C SPI NV services (SEV state), hence SME/SEV by default.
- Firmware-first RAS (HEST, GHES, BERT, EINJ) and the TPM description.
- xHCI root hub port descriptions (`_UPC`/`_PLD`): the board's port map isn't
  known.
