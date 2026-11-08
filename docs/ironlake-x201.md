# Ironlake / Ibex Peak and the ThinkPad X201

This port brings coreboot's `northbridge/intel/ironlake`,
`southbridge/intel/ibexpeak`, `cpu/intel/model_2065x` and
`mainboard/lenovo/x201` to fstart. Chipset code is shared: another Arrandale
or Clarkdale board (T410, ms2290, a desktop with
`IronlakeIbexPeakConfig::desktop()`) only needs a board crate. An X201 with
an i7 M620 and 8 GiB of RAM now boots Linux through CrabEFI and passes a
10-second RTC-timed S3 resume; other board bindings remain untested.

## Where the code lives

| coreboot | fstart |
| --- | --- |
| `cpu/intel/model_2065x` (MSRs, turbo, `CpuDriver`, SMM save state) | `crates/arch/src/x86/cpu/intel/model_2065x.rs` |
| NEM CAR on Nehalem-class cores, `0x2e0` teardown | `crates/arch/src/x86/boot/car.rs`, `car_teardown.rs` |
| `cpu/intel/model_2065x/acpi.c` (P/C/T-states, `\PPKG`) | `crates/driver-intel/src/cpu/model_2065x_aml.rs` |
| `northbridge/intel/ironlake` early init, PAM, DEVEN, DMI, SMRAM, resume | `crates/driver-intel/src/ironlake/mod.rs` |
| `raminit.c`, `raminit_tables.c`, `quickpath.c` | `crates/driver-intel/src/ironlake/raminit/` |
| `gma.c` (func0 init, post-VBIOS PM, SWSCI, OpRegion) | `crates/driver-intel/src/ironlake/igd.rs` |
| `acpi/{ironlake,hostbridge,uncore}.asl`, MCFG | `crates/driver-intel/src/ironlake/acpi.rs` |
| `southbridge/intel/ibexpeak` bootblock/early PCH, CIR, USB, thermal | `crates/driver-intel/src/ibexpeak/early.rs` |
| `lpc.c`, `pch.c`, `usb_ehci.c`, `azalia.c`, `sata.c`, `smbus.c`, `thermal.c`, `spi.c`, finalize | `crates/driver-intel/src/ibexpeak/ramstage.rs` |
| `me.c`, `early_me.c` (HECI, UMA, MKHI) | `crates/driver-intel/src/ibexpeak/me.rs` |
| `bd82x6x/acpi/*.asl` as used by Ibex Peak, PCIe hot-plug SSDT | `crates/driver-intel/src/ibexpeak/acpi.rs` |
| `smihandler.c` (sleep, DRAM reset gate, S5) | `crates/driver-intel/src/southbridge/smi.rs` (shared ICH/PCH) |
| Chipset policy builder and stage binding | `crates/platform-intel/src/ironlake.rs` |
| `ec/lenovo/h8` + `mainboard/lenovo/x201/{dock.c,smihandler.c,acpi}` | `crates/driver-lenovo/src/x201/`, `thinkpad.rs` |
| `mainboard/lenovo/x201` devicetree, GPIO, HDA, romstage | `boards/lenovo/x201/` |

Generic pieces this port added to the shared crates: `IntelSouthbridgeDriver`
`pre_raminit_init`/`chip_init`/`program_subsystem_id` hooks, the ICH/PCH
sleep-SMI policy, the PCH-placed backlight registers of the GFX0 node,
`CpuDriver::logical_cpu_count`, `platform_info_tsc_frequency_hz`, the Ibex
Peak entry in the SPI memory-cache controller and PS/2 nodes with board
EISA IDs.

## Boot flow

The bootblock follows coreboot's `mainboard_romstage_entry()`:
`ironlake_early_initialization()` and the PCH bootblock work before the
console; SMBus, S3 detection and the bootstrap windows with the console;
then the board's clock-generator write, GPIO/CIR/interrupt map/thermal
(`pre_raminit_init`), `chipset_init()` and `raminit()`. The first cold pass
reconfigures QuickPath and needs coreboot's CPU-only reset (MCHBAR
`0x1af0` bit 4, scratchpad `0x2ca8`); the bootblock then runs again and
raminit continues. The ME UMA handshake and the DMI virtual channels follow
training, as in coreboot.

The ramstage uses the shared fixed Intel flow: PCI allocation with the
EHCI debug BAR and chipset BARs kept, `chip_init` (DEVEN, PCH LPC/PM/USB/
HDA/SATA/PCIe/thermal), MP init with the model 2065x driver, SMM in TSEG,
the native modeset, ACPI/SMBIOS, and the PCH finalize (SPI opcode menu
and chipset lockdown).

## Differences from coreboot

* The X201's clock-generator write (coreboot `mainboard_pre_raminit()`)
  runs just before raminit's `chipset_init()` instead of after it; both
  precede the first SPD read and the QuickPath reset. The write is the
  same: the first five registers back, with the FSB word in the first two.
* PCH GPIOs are programmed at the start of training rather than in
  `early_pch_init()` before S3 detection. The X201 reads no GPIO earlier.
* CMOS options are compile-time defaults from `cmos.default`.
* There is no VBIOS: the IGD is set up by the native modeset, so the
  INT15 hooks are absent.
* The `MCHC` CTDP methods coreboot inherits from Sandy Bridge are omitted:
  Ironlake has no configurable TDP behind them.
* The TPM is probed and described (`PNP0C31` under `\_SB.PCI0`) like
  `drivers/pc80/tpm`. On S3, the shared TPM driver sends `Startup(STATE)`
  before returning to Linux, independently of measured boot. Measured boot
  is off, as in coreboot's default X201 configuration.
* Unlike coreboot's old ME S3 policy, MEI remains visible on resume. The
  cold-boot MEI handshake is skipped and Linux restores its existing device.
* `UNCR` acknowledges PCI `_OSC` capabilities as `PCI0` does, so its legacy
  PCI-root description does not cause Linux to disable ASPM ownership.
* vboot, CFR and the EDK2 SMMSTORE options are coreboot-specific and not
  ported.

## Known gaps

* S1 is not advertised: its returning SMI path does not yet re-arm sleep
  interception, which must remain active for the next S3 transition.
* Native raminit supports one DIMM per channel. Second-slot SPD entries
  are rejected by the platform builder and the northbridge driver.
* The dock's DisplayPort (libgfxinit `DP2`): `fstart-intel-gma` has no PCH
  DisplayPort path for Ironlake yet. The port stays in the X201 output list
  and is skipped until it does; LVDS, VGA and HDMI are driven.
* Only the i7 M620 / 8 GiB X201 configuration has been exercised. VGA,
  HDMI, dock behavior, the X201T digitizer, other CPU/DIMM combinations,
  repeated sleep cycles and non-RTC wake sources still need validation.
* Firmware SPI writes remain enabled for the training cache and debug log.
  Production flash protection and a provisioned signing trust anchor are
  outside this development bring-up.

## ExpressCard SMBIOS

The ExpressCard slot now uses the shared [SMBIOS Type 9 support](smbios-slots.md).
The board supplies coreboot's physical description (type 7, short length 3,
x1 width encoding 8) and its upstream bridge, 00:1c.3. The generic PCI
helper derives usage from enumeration; merely finding the root port does
not make the slot occupied.

Unlike coreboot's current writer, the emitted PCI association identifies
the card on the bridge's assigned secondary bus, not the root port. Empty
or unknown slots retain their physical description with an unknown PCI
address. This follows DSP0134 section 7.10.8. USB-only ExpressCards cannot
be detected from PCI enumeration, and SMBIOS is a boot-time snapshot.

## Rebase and source comparison

Rebased the X201 change onto fstart `master` at `774ccc1d` (the merged
GM45/X200 work). The reference checkout is `/home/arthur/src/coreboot` at
`1958b5d923a`; this is a local-source comparison, not a claim against the
latest upstream coreboot or proof of hardware equivalence.

The rebase preserves master's removal of `driver-lenovo::x200` (that
policy now lives in the X200 board). X201 no longer depends on it. Its
H8/dock AML uses `PcNamespace`, the Ironlake drivers supply their ACPI
names, and Ibex Peak implements the current southbridge ACPI interface.
The shared GMCH graphics path retains its northbridge-supplied device name
while Ironlake selects the PCH backlight layout.

Checks against that coreboot tree:

* All 16 arrays in `raminit_tables.c` match the Rust tables, including
  nested shape and all 2,723 integer entries. The board VBT is
  byte-identical.
* The top-level raminit sequence retains the scratchpad/GEN_PMCON_2
  recovery, QuickPath CPU-only reset, cold JEDEC/training versus S3 timing
  replay, final memory map, and ME UMA setup ordering. The clock-generator
  and GPIO ordering differences above remain; this is not an exhaustive
  equivalence check of every timing/training expression.
* The ME UMA message is the same 22-byte request (command 0x08, field
  0x82, 64-bit address and MiB size); fstart bounds waits that coreboot
  leaves unbounded. fstart's receive loop consumes every reply dword,
  rather than copying coreboot's double increment of its loop index.
* The board's dock links, EC events (0x18/0x27/0x50 disconnect,
  0x37/0x58 connect), 250-ms dock delay, ACPI channel switch, and S3 wake
  mask 0x14 follow `dock.c` and `smihandler.c`.
* Board tests cover GPIO, HDA, flash/SPD facts, the five-byte clock-generator
  write, and a complete DSDT disassembly/recompile with iasl.

The dock DisplayPort gap listed above is still present. Hardware bring-up
subsequently verified cold boot, RAM publication, LVDS modesetting, the
four actual LAPIC IDs and cache persistence. A 10-second RTC-timed S3 test
returned to the same Linux session with four CPUs, MEI enabled and all 24
TPM PCRs readable, without MEI or TPM resume errors. This is limited
single-machine evidence, not exhaustive validation of DRAM training, ME
firmware behavior or SMM hardware effects.

Rebase validation passed: arch, Intel (including GM45/ICH9 regression),
platform and Lenovo host tests; all five X201 board tests; and the release
halt-payload build and 8-MiB image assembly. The two required microcode
files were copied from the clean main workspace submodule at the pinned
revision `98f8d817ca3d560c48ae988bd805d1b53b48a631`. Assembly used an
ephemeral development signing key. Vendor descriptor/GbE/ME contents are
still not bundled; the resulting image is not a replacement for an
actual-machine flash backup.
