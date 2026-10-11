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

## References

- coreboot `cpu/intel/socket_LGA775`, `cpu/intel/model_6fx` and
  `mainboard/asus/p5gc-mx/early_init.c`.
- Intel Core 2 model-specific register definitions in Intel's
  `MdePkg/Include/Register/Intel/Msr/Core2Msr.h` (EDK II; SDM Vol. 4 reference):
  IA32_MISC_ENABLE TM2 requires CPUID.1:ECX[8]; EIST uses CPUID.1:ECX[7].
- Intel ICH7 Family Datasheet §7.1.57 (clock gating) and §10.7.5 (CF9 reset).
- Winbond W83627DHG V1.4 §20 for GPIO, mux and activation semantics.
