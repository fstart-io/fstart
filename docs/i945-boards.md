# i945 board-port foundations

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
supported Core 2 models; the mainboard will supply its physical wiring to the
W83627DHG driver. The driver owns muxes, direction, inversion and latch fields,
including preservation of the read-only ACPI strap. GPIO initialization is not
glitch-free runtime switching.

BSEL configuration must skip S3, compare the requested fields and request a
full reset after changes. The shared i945 full-reset operation and ICH7
PCIe-root-port-only clock-gating operation keep these register encodings out
of board code. Clock gating belongs in the post-training board hook, before
normal mainstage chipset policy.

These are reusable foundations, **not a completed P5GC-MX port**. GPIO
readback, reset retention, DRAM training and S3 behavior need hardware
validation. Build or unit-test success does not establish any of them.

## References

- coreboot `cpu/intel/socket_LGA775`, `cpu/intel/model_6fx` and
  `mainboard/asus/p5gc-mx/early_init.c`.
- Intel Core 2 model-specific register definitions in Intel's
  `MdePkg/Include/Register/Intel/Msr/Core2Msr.h` (EDK II; SDM Vol. 4 reference):
  IA32_MISC_ENABLE TM2 requires CPUID.1:ECX[8]; EIST uses CPUID.1:ECX[7].
- Intel ICH7 Family Datasheet §7.1.57 (clock gating) and §10.7.5 (CF9 reset).
- Winbond W83627DHG V1.4 §20 for GPIO, mux and activation semantics.
