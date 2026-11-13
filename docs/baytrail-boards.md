# Bay Trail boards

Reference for the Intel Atom E3800 / Celeron J1900 / N2xxx SoCs (Bay Trail,
Silvermont cores) with native DDR3 raminit.

- Platform: `Baytrail` in `crates/platform-intel/src/baytrail.rs` (feature
  `baytrail`), configured with `BaytrailConfig`.
- North cluster (`IntelNorthbridgeDriver`): `BaytrailNorth` in
  `crates/driver-intel/src/baytrail/north.rs`. Owns the IOSF sideband
  (`iosf.rs`), B-unit memory map (`bunit.rs`), P-unit start (`punit.rs`),
  native raminit (`raminit/`), TXE UMA handshake (`txe.rs`), SMRAM and the
  PCI root.
- South cluster (`IntelSouthbridgeDriver`): `BaytrailSouth` in `south.rs`.
  PMC (`pmc.rs`), LPC/ILB (`lpc.rs`), GPIO (`gpio.rs`), IRQ routing
  (`irq.rs`), SMI (`smi.rs`, reuses the ICH SMM handler), and the PCI
  functions in `devices/`: GFX, HDA, LPE, LPSS, SCC (eMMC/SDIO/SD), SATA,
  USB (EHCI/xHCI), PCIe root ports, ModPHY and the perf/power settings.
- CPU: `crates/arch/src/x86/cpu/intel/silvermont.rs` (MSRs, turbo,
  `set_max_freq`); ACPI `_PSS`/`_CST`/`_PSD` in
  `crates/driver-intel/src/cpu/silvermont_aml.rs`.
- DDR3 SPD decode: `crates/driver-spd/src/ddr3.rs`.
- Board crate: `boards/intel/minnowmax` (`-b intel-minnowmax`).

The code follows coreboot's `soc/intel/baytrail` and its native raminit
series (Gerrit CB:92940 to CB:92948 and the DDRIO rework in CB:97012 and
CB:97013, plus the fused-frequency limit of CB:97004).

## Build

```sh
cargo fbuild assemble -b intel-minnowmax --release --payload halt
cargo fbuild assemble -b intel-minnowmax --release --payload linux \
    --kernel bzImage \
    --linux-bootargs "console=ttyS0,115200"
```

The output is the full 8 MiB IFD image. Program only the BIOS region; the
descriptor and the TXE region (0x1000..0x3fffff) must stay untouched:

```sh
flashprog -p dediprog -c "W25Q64BV/W25Q64CV/W25Q64FV" --ifd -i bios -w image.bin
```

## Boot flow

| Stage | Bay Trail work |
| --- | --- |
| bootblock | NEM CAR at `0xfe000000` (64 KiB), MMCONF via CF8 then ECAM, LPC/ILB decode, TCO halt, COM1 |
| romstage (bootblock binary) | P-unit start, GFX stolen sizes, SPD by board strap, TXE UMA size, native raminit, memory map and IMRs, TXE DRAM Init Done |
| postcar | MTRRs from the B-unit map |
| ramstage | ModPHY, SCC, function disables, PCIe/SATA/USB/LPSS/LPE/HDA init, GFX power wells, perf/power table, MP + microcode, SMM (SMRR protected TSEG), ACPI/SMBIOS/OpRegion |

## MinnowBoard MAX

| Item | Value |
| --- | --- |
| SoC | Atom E3845 (CPUID `0x30679`, D0), 4 cores, 83.3 MHz bus |
| Memory | 1 or 2 GiB DDR3L memory-down, SPD selected by the BOM strap GPIOs |
| Flash | 8 MiB W25Q64 with IFD; BIOS at `0x400000` |
| Console | COM1 `0x3f8`, 115200 8N1 |
| Memory map (2 GiB) | TOLM 2 GiB; TXE UMA 16 MiB at 2032 MiB, GSM 64 MiB at 1968 MiB, GTT 2 MiB at 1966 MiB, TSEG 8 MiB at 1952 MiB |

Verified on hardware in two stages:

- Pre-rebase image (2026-11-04, `v22`): native raminit at DDR3-1333 CL9, all
  four CPUs, SMM, ACPI, SMBIOS (DMI), and Linux 7.3 with AHCI, xHCI, EHCI,
  SDHCI, LPSS UARTs and the i211 NIC enumerated, `intel_idle` C6N/C6S on
  every core.
- This stack, rebased onto master (2026-11-13, `--payload halt`): bootblock,
  native raminit and DDR3-1333 CL9 training, the TXE UMA handshake, postcar,
  ramstage, four CPUs with microcode, SMM relocation and lock, ACPI and
  SMBIOS emission, the IGD OpRegion, and the payload handoff.

A Linux payload has not been re-run on the rebased stack.

## Known limitations

- No S3 resume: the native raminit has no self-refresh exit path.
- Raminit takes single-rank, unbuffered, 64-bit DDR3 only (rank 0 is the
  only one the PHY setup and training cover).
- The B-unit stops answering host sideband reads during ramstage device
  init. `BaytrailNorth` latches BMBOUND/BMBOUND_HI before the bus scan and
  uses the latched values afterwards (ACPI `_CRS`, e820).
- No chipset options yet: both clusters declare an empty option set, so the
  setup menu offers no SATA mode, NMI, graphics memory (UMA) or XDCI control.
  Those values are board policy in the board's `config.rs` and `GfxConfig`;
  they are not user-settable through `fstart_options` as they are on the ICH
  and IGD platforms.
