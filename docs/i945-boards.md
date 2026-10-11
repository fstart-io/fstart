# i945 boards

The i945/ICH7 flow is shared across CPU packages. Existing D945GCLF and X60
bindings retain their Atom and mobile Core policies respectively.

`Lga775Core2` adds the desktop Core 2 / Celeron model-6FX package policy, with
coreboot's default 32 KiB CAR geometry and matching microcode selections. It
**does not** support Netburst F3X/F4X or Enhanced Core 1067X. Those require
separate CPU initialization, not merely adding microcode names. Desktop setup
retains the CPU's performance-state selection, enables only advertised TM2/EIST
features, and does not request mobile C-state, C4E, EMTTM or VID policy.
Feature-control MSR access is gated before the read on VMX/SMX or a supported
alternative-SMRR requirement, so non-VMX/SMX Celerons are not probed.

## ASUS BSEL boundary

P5GC-MX connects BSEL0 to GP32, BSEL1 to inverted GP33 and BSEL2 to GP55.
`Core2CpuDriver::bus_select` reads the CPU's requested strap levels only on
supported Core 2 models; the mainboard supplies its physical wiring to the
W83627DHG driver. The driver owns muxes, direction, inversion and latch fields,
including preservation of the read-only ACPI strap. GPIO initialization is not
glitch-free runtime switching.

BSEL configuration skips S3 before reading the CPU MSR or programming GPIO.
Data, inversion and mux changes request a full reset; direction/activation-only
initialization does not itself request another reset. The shared i945 full-reset
operation and ICH7 PCIe-root-port-only clock-gating operation keep these register
encodings out of board code. Clock gating uses `after_memory_training`, before
DMI/post-DRAM chipset setup, matching the reference's pre-late-init position.
Normal mainstage chipset gating policy still runs later.

## Experimental P5GC-MX selection

`cargo fbuild build -b asus-p5gc-mx --release --payload halt` builds the stages;
`cargo fbuild assemble -b asus-p5gc-mx --release --payload halt` assembles a
512 KiB image. The halt image fits the stock capacity; this does not establish
that a larger payload fits. The board uses the shared i945/ICH7 graphics,
DDR2, PCI, SMM and ACPI flows with desktop model-6FX CPU policy.

Only COM1 is initialized before RAM. Mainstage configures COM2, parallel and
keyboard/mouse resources, floppy/DMA, monitoring and the DRAM standby gate.
The GPIO4/UART-B mux is used for COM2 instead of copying the reference's
unrelated GPIO4 output writes. Unlisted Super I/O pins are preserved rather
than assigning guessed functions or replacing whole GPIO register banks.

Remaining limitations:

- Netburst and Enhanced Core CPUs are unsupported; this is not full LGA775 coverage.
- S3 is disabled: this board does not enable the training/stage cache, so the
  fixed flow rejects resume before the BSEL hook. Its S3 guard is defensive,
  not evidence of a working resume path.
- The reference's unusual 1458 PCI subsystem IDs are not reprogrammed; native
  hardware values remain. No board-ID overwrite API exists in this flow yet.
- Floppy resources are programmed but the generic Super I/O AML does not
  declare a floppy device; OS enumeration remains incomplete.
- Parallel DMA3 is programmed without changing the controller mode. This does
  not establish ECP support; generic AML describes a standard LPT port.
- GPIO readback/reset retention, DRAM training, S3, display and peripheral
  behavior are unvalidated. A reset loop remains a hardware bring-up risk.

Keep a vendor ROM backup and external programmer before attempting a boot.
This is **not hardware-validated firmware** or a completed port of every
i945 board. Build and unit-test success do not establish hardware correctness.

## W83627EHG foundation for MB899

`driver-superio::w83627ehg` identifies only EHG IDs 886xh and provides UART,
PS/2 and parallel resources plus typed clock, VID-threshold, fan-pad electrical
mode and pin routing. Hardware-monitor allocation does not configure fan duty
or start monitoring; MB899 still needs a separate runtime monitor/fan binding.
This foundation is not an MB899 board selection or hardware validation.

GPIO3/4/5 reuse the verified DHG mechanism, including direction-before-data,
field preservation and strap-change classification. EHG GPIO1 uses LDN7,
requires all eight pads explicitly configured before its game-port mux changes,
and preserves MIDI/GPIO6 activation. DHG rejects GPIO1 requests before I/O.
GPIO4's bank-wide UART-B conflict is unchanged; unrequested controls, not all
bank-wide routes, are preserved. GPIO2/6 programming remains unsupported.

The MB899 reference contains reserved-register writes and a `CR30=0x03`
comment claiming GPIO3+4. EHG V1.3 §7.10 describes that value as GPIO2+3;
GPIO4 is bit 2. These bytes/comments are not evidence of otherwise-unknown
wiring and must not be blindly copied into the eventual binding.

## Experimental Gigabyte GA-945GCM-S2L/S2C selections

The shared crate in `boards/gigabyte/ga-945gcm-s2l` provides two selections:
`gigabyte-ga-945gcm-s2l` and `gigabyte-ga-945gcm-s2c`. Both use 512 KiB flash,
i945GC/ICH7, desktop model-6FX CPUs, the same VBT, and IT8718F resources.
The S2C variant is selected by Cargo metadata, not by runtime board guessing;
its SMBIOS product name differs from S2L.

Before DRAM, the two populated fans are forced full-speed and the board's
physical Super I/O pins are configured, after the framework detects the boot
path. Pre-console setup touches only COM1. Mainstage enables PS/2 and standard
LPT; after console registration it prepares analog monitoring with full-speed
cooling, waits two seconds using ICH7's initialized HPET, checks sampling/channel
readback and temperature plausibility, then releases both CPU-diode fan curves.
Qualification failure logs a warning and leaves both fans full-speed. Clock gating runs immediately
after memory training. GPIO event destinations are semantic SMI/SCI policy;
they do not themselves enable event gates, and GPE0 remains disabled.

The environmental-controller integration accepts only the verified IT8718F
C-version code (1); other revision codes fail rather than assuming register
compatibility. Reserved register writes and the undocumented vendor EF reboot
write are omitted. Unlisted Super I/O pins/latches are preserved; unused
UART2 pads are explicitly GPIO inputs. The 35–75°C, zero-start-duty curve uses
a ceiling-quantized slope derived from its nominal endpoints, not the
reference's raw slope or reserved full-temperature registers. Sampling,
hysteresis and smoothing mean this is not an unconditional thermal deadline.
Startup qualification requires readings strictly inside the configured alarm
limits (here 0–127°C), excluding signed endpoints. This is conservative range
checking, not proof of freshness or detection of every electrical fault: a
plausible stuck reading or later sensor failure can escape it.

Remaining limitations:

- No hardware validation: DRAM, pin routing, sensor readings, polarity, fan
  startup/response and peripherals require controlled bring-up. Keep an
  external programmer, vendor ROM backup and independent temperature checks.
- S3 is disabled; the early-hook S3 guard does not establish resume support.
- Netburst/Enhanced Core CPUs remain unsupported.
- S2L's RTL8168-specific reset/MAC initialization is missing; a warning is
  emitted only for that variant. The reference does not request it for S2C.
- PCI subsystem-ID overrides are not applied. Floppy and COM2 are disabled;
  LPT is standard SPP with no ECP/DMA support.
- Halt-image builds do not establish larger-payload fit or thermal safety.

## References

- coreboot `cpu/intel/socket_LGA775`, `cpu/intel/model_6fx` and
  `mainboard/asus/p5gc-mx/early_init.c`.
- Intel Core 2 model-specific register definitions in Intel's
  `MdePkg/Include/Register/Intel/Msr/Core2Msr.h` (EDK II; SDM Vol. 4 reference):
  IA32_MISC_ENABLE TM2 requires CPUID.1:ECX[8]; EIST uses CPUID.1:ECX[7].
- Intel ICH7 Family Datasheet §7.1.57 (clock gating) and §10.7.5 (CF9 reset).
- Winbond W83627DHG V1.4 §20 for GPIO, mux and activation semantics.
- coreboot `mainboard/gigabyte/ga-945gcm-s2l`, including both variant selections.
- ITE IT8718F V0.3 §§8.3, 8.11 and 9.6: physical pin functions, C-version
  identification, analog monitoring and seven-bit SmartGuardian curves.
