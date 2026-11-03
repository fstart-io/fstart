# Sandy Bridge / bd82x6x and the ThinkPad X220

This port brings coreboot's `northbridge/intel/sandybridge`,
`southbridge/intel/bd82x6x`, `cpu/intel/model_206ax` and
`mainboard/lenovo/x220` (variant `x220`) to fstart. Chipset code is shared:
another Sandy Bridge or Ivy Bridge board with a 6 or 7 Series PCH only needs
a board crate. The reference is the local coreboot checkout at
`1958b5d923a`; the comparison is source level. **Nothing here has booted on
hardware yet.**

## Where the code lives

| coreboot | fstart |
| --- | --- |
| `cpu/intel/model_206ax` (MSRs, VR limits, turbo, power limits, `CpuDriver`, SMM facts with IED) | `crates/arch/src/x86/cpu/intel/model_206ax.rs` |
| `cpu/intel/model_206ax/acpi.c` (P/C/T-states) | `crates/driver-intel/src/cpu/model_206ax_aml.rs` |
| `northbridge/intel/sandybridge` early init, DMI, VT-d, memory map, finalize | `crates/driver-intel/src/sandybridge/mod.rs`, `regs.rs` |
| `raminit*.c` (native DDR3 training, fast boot record) | `crates/driver-intel/src/sandybridge/raminit/` |
| `gma.c` (GT power management, power meter) | `crates/driver-intel/src/sandybridge/gma.rs`, `powermeter.rs` |
| `acpi/*.asl`, `acpi.c` (PCI0, PEG, MCFG, DMAR) | `crates/driver-intel/src/sandybridge/acpi.rs` |
| `southbridge/intel/bd82x6x` bootblock, early PCH, USB, IOBP, DMI, thermal | `crates/driver-intel/src/bd82x6x/early.rs` |
| `pch.c`, `pcie.c`, `sata.c`, `usb_*.c`, `azalia.c` | `crates/driver-intel/src/bd82x6x/devices.rs` |
| `lpc.c` (PM init, RTC, HPET, clock gating) | `crates/driver-intel/src/bd82x6x/lpc.rs` |
| `early_me.c`, `me.c`, `me_common.c`, `me_smm.c` | `crates/driver-intel/src/bd82x6x/me.rs` |
| `fadt.c`, `acpi/*.asl` | `crates/driver-intel/src/bd82x6x/acpi.rs` |
| Chipset policy builder and stage binding | `crates/platform-intel/src/sandybridge.rs` |
| `mainboard/lenovo/x220` devicetree, GPIO, HDA, SMI handler, ACPI | `boards/lenovo/x220/` |
| `drivers/ricoh/rce822`, `drivers/i2c/at24rf08c` lock, `drivers/usb/acpi` | `crates/driver-ricoh/src/rce822.rs`, `driver-lenovo` `eeprom::lock_rfid`, `crates/acpi/src/usb.rs` |

The CPU is part of the system agent, so like Ironlake the platform binding
supplies CAR (`0xfefe0000`, 128 KiB), microcode (`06-2a-07`, `06-3a-09`) and
the MP driver itself.

The drivers follow the current Intel layout: every chipset is built, the
board's platform binding picks one; there are no chipset Cargo features.
The platform builder mirrors the Ironlake one (`.disable([...])` with
`PchFunction`, `.pcie_attached()`, `.ehci_debug_controller()`,
`.lpc_generic_io()`, `.hda_verb()`), the drivers expose the shared user
options (`gfx_uma_size`; `power_on_after_fail`, `sata_mode`, `nmi`) and the
northbridge describes its root bridge as resources (`pci_root()`); PCI0's
`_CRS` and `PDRC` come from it.

Pieces shared with the other chipsets instead of copied:

* `igd::pch`: the split-PCH GPU devicetree policy (`PchIgdConfig`,
  `PanelPort`), the panel/backlight half of `gma_pm_init_post_vbios()` and
  `gma_enable_swsci()`, used by Ironlake and Sandy Bridge.
* `southbridge::acpi`: the rate-matching-hub EHCI node and the PCIe hot-plug
  GPE generator, used by Ibex Peak and the 6/7 Series PCH.
* `southbridge::spibar`: the ICH9-style SPI block (opcode menu, VSCC,
  `FLOCKDN`); `southbridge::spi` (the memory cache and flash log) also
  drives Cougar Point and Panther Point.
* `southbridge::ehci_debug`: takes the northbridge's ECAM bus count, so
  the debug BAR fits below Sandy Bridge's 64-bus ECAM at `0xf0000000`.
* `driver-lenovo::h8`: the EC wake register (`H8::wake_armed`) X201 and
  X220 both use in SMM.

## Boot flow

The bootblock enables PCIEXBAR (64 buses at `0xf0000000`), the PCH decode,
BARs, SPI speed and the CPU flex-ratio/SMT soft-reset straps before the
console. With the console up, early init runs `systemagent_early_init`,
GbE, SMBus and `early_usb_init`; the board locks the AT24RF08C RFID side.
`train_memory` then programs the board GPIOs (`pre_raminit_init`), runs
the DMI link bring-up coreboot does inside `perform_raminit` (PCH IOBP and
DMI, system-agent DMI recipe and training, both ends' virtual channels,
`early_thermal_init`, the ME UMA wait and TXT clear) and native raminit.
Fast boot and S3 replay the `SNB1` training record from the memory cache.

The ramstage uses the shared fixed Intel flow: function enables, root port
coalescing and the SATA map before enumeration; PCI allocation with the
EHCI debug BAR kept when selected; system-agent init (DMI, BIOS_RESET_CPL,
power limits), PCH LPC/PCIe/USB/SATA/HDA/ME init; MP with the model 206AX
driver and SMM with a 4 MiB IED in an 8 MiB TSEG; the GT power management
and native modeset; ACPI/SMBIOS; and the finalize locks.

## Differences from coreboot

* The DMI link and thermal init run after the board GPIOs, inside the DRAM
  phase; coreboot programs GPIOs in `early_pch_init()` before the console.
* The finalize locks run from the ramstage on cold boot and resume rather
  than from the `APM_CNT_FINALIZE` SMI. Like the Ibex Peak binding, this
  development configuration keeps firmware SPI writes open (BLE/SMM_BWP
  clear) for the training cache, the flash log and payload writes.
* The RCE822 function-disable mask is written after allocation, so the
  allocator still sizes the functions it hides.
* `cmos.layout` options use the platform's CMOS option store by key, not
  coreboot's bit layout. `hyper_threading` and the PCIe root port enables
  remain board configuration.
* S1 is not advertised, as on Ibex Peak.
* The PCH DisplayPort outputs (`gma-mainboard.ads` DP1..3, the dock) are
  not selected; LVDS, VGA and HDMI are.
* TPM: probed, described (`PNP0C31`) and restored on S3; no measured boot.
* xHCI (Panther Point): node, routing registers and `_PRW` only. The
  `_OSC` switch and the PPT A0 S3 workaround do not apply to the X220.

## Build

```sh
cargo fbuild assemble --board lenovo-x220 --release --payload uefi --debug ehci
```

`--debug ehci` uses EHCI2 (00:1a.0, coreboot `USBDEBUG_HCD_INDEX=2`) with
its BAR at `0xfef00000`. The image describes the stock 8 MiB layout but
bundles no descriptor, GbE or ME contents: flash the BIOS region only, and
keep a verified backup of the machine's flash and an external programmer.

## Validation

Host tests cover the raminit tables and record, the PCH and system-agent
policy checks, the default interrupt routing, the PEG `_PRT` rotation, the
board's GPIO table against `variants/x220/gpio.c`, the HDA pin defaults,
the PCH function enables and a complete DSDT disassembly/recompile with
iasl. No flashing, hardware boot or resume test was performed.
