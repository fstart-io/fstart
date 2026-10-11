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
or start monitoring. The separate `winbond_hwm::WinbondEhgMonitor` implements
EHG V1.3 polling sensors and manual full-speed output. The experimental MB899
binding below uses these foundations; neither driver has hardware validation.

GPIO3/4/5 reuse the verified DHG mechanism, including direction-before-data,
field preservation and strap-change classification. EHG GPIO1 uses LDN7,
requires all eight pads explicitly configured before its game-port mux changes,
and preserves MIDI/GPIO6 activation. DHG rejects GPIO1 requests before I/O.
GPIO4's bank-wide UART-B conflict is unchanged; unrequested controls, not all
bank-wide routes, are preserved. GPIO2/6 programming remains unsupported.

The MB899 reference contains reserved-register writes and a `CR30=0x03`
comment claiming GPIO3+4. EHG V1.3 §7.10 describes that value as GPIO2+3;
GPIO4 is bit 2. These bytes/comments are not evidence of otherwise-unknown
wiring and are not blindly copied into the board binding.

### EHG runtime monitor boundary

The runtime driver uses the shared typed indexed-PIO register accessor and
restores bank/vendor/beep selection after banked accesses, including failed
identity checks. Documented must-zero bank bits are cleared; unrelated sensor
divisors and output controls are preserved. PnP EHG identification is required
before its runtime vendor/chip sanity check: these IDs do not establish model
compatibility on their own.

Named sensor inputs describe thermistor/diode selection and signed Celsius
offsets. Full-speed outputs explicitly select PWM or DC voltage according to
board wiring and use the documented maximum value; this does not prove actual
fan speed or safe electrical wiring. Control mode precedes value programming,
so takeover from inherited automatic control is not glitch-free. Unrequested
outputs are unchanged. GPIO fan-pad mux/buffers, PWM frequencies and tachometer
divisors remain separate policy; no thermal/speed cruise is enabled.

Initialization programs cooling before sensors and starts conversions last.
Monitoring is polling-only: SMI, OVT and global beep outputs are disabled, not
claimed as an autonomous thermal shutdown mechanism. Whole-degree signed
readings intentionally omit remote sensors' half-degree bit, avoiding a torn
two-byte sample. The caller must allow conversion settling and cannot infer
freshness or sensor health from a plausible value. This has no MB899 runtime or
hardware validation yet.

## Experimental IBASE MB899 selection

`cargo fbuild build -b ibase-mb899 --release --payload halt` and
`cargo fbuild assemble -b ibase-mb899 --release --payload halt` select Socket M,
i945GM/ICH7-M and W83627EHG at PnP 4Eh, with a 512 KiB FWH image. Protected-mode
stages include Core Duo/Yonah, not just long-mode-capable Core 2 CPUs. The board
uses the reference's default SPD addresses 50h–53h and 768 MiB PCI MMIO window.

Pre-console setup owns COM1 and the 48 MHz clock input. After boot-path detection,
SYSFANOUT and CPUFANOUT0 are explicitly full-speed PWM/open-drain outputs,
matching the reference's CR04=0 manual/PWM selection and CR24 pad controls.
The runtime HWM window stays at 290h rather than temporarily using A00h. GPIO3
uses only verified GP32–34: GP32 is low before RAM and high in mainstage; GP33/34
are inputs. Their roles are unknown—no guessed reset/strap signal is assigned.
COM2 and PS/2 resources are added in mainstage; GPIO4 remains UART B. The
secondary CPU fan mux displaces MIDI, which is not advertised as supported.
Unlisted GPIO controls, GPIO2 activation and reserved reference writes are not
copied. Floppy, parallel, serial flash and watchdog outputs are disabled.

Post-console monitoring selects the reference's analog diode types and signed
offsets (-15/+25/-4°C), with battery sampling. Cooling stays manual full-speed;
there are no automatic fan curves, sensor-based speed decisions or monitoring
interrupts. Inherited PWM frequency and tachometer-divisor settings remain;
this does not validate actual fan speed, polarity, calibration or sensor health.

RCBA interrupt routing follows `early_init.c`; PCI-bridge _PRT follows the
board's ACPI wiring. Old hand-written PIRQ/MP tables disagree in places and are
not copied as competing sources. Shared PCI enumeration/table generation is
used instead, but optional riser bridge discovery/swizzling remains unvalidated.
The unexplained 300h LPC decode is omitted; 290h monitoring has its own window.

Remaining limitations:

- No hardware validation, including GPIO phase transition, cooling, DRAM,
  CPU/interrupt/peripheral behavior or FWH boot. Keep an external programmer,
  vendor ROM backup and independent temperature/fan observation.
- S3 is disabled; no board training/stage cache is configured. The early cooling
  hook guards the detected boot path, not the always-Normal pre-console context.
- VGA-only native display fallback: no verified board VBT, OpRegion or LVDS
  panel timings. The reference's LVDS INT15 hint is not enough to invent them.
- Reference EC AML advertises 62h/66h/GPE23 without usable query/transport policy;
  no fictional EC is emitted. MIDI/game-port, ACPI-controller and GPIO2/6 policy
  remain incomplete. Generic AML covers only the configured UART/PS2 resources.
- AC97 audio/modem functions are retained but not initialized or validated; no
  HDA codec verbs are supplied. PEG disable policy is not yet represented by the
  shared i945 config. Only the four supported ICH7-M PCIe ports are configured.
- Optional risers, PCI subsystem-ID overrides and legacy OS table equivalence
  are incomplete. Halt-image fit does not establish larger-payload fit.

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
