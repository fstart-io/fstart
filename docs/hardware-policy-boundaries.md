# Hardware policy at shared-code boundaries

Shared algorithms consume the resources and policy selected by a platform;
knowing a particular board's name or largest buffer is not an interface.
The X201 training-cache refactor established that rule for CAR storage. These
follow-ups apply it elsewhere without introducing another hardware-access trait.

## ACPI shutdown

`Description::acpi_s5` carries PM1 control ports and their unshifted `_S5` values.
Intel obtains the port from the same southbridge configuration as the FADT;
AMD/SP3 use their ACPI facts. Q35 uses its programmed PMBASE (`0x600`), not the
old adapter's guessed `0x400`. The x86 mechanism uses `PioRegister` and Tock
fields, preserves unrelated PM1 bits, programs PM1a/PM1b types before SLP_EN,
and halts instead of rebooting when shutdown is unsupported or returns.

This configures fstart's **boot-lifetime CrabEFI reset adapter**. The pinned
separate CrabEFI runtime image has its own reset ABI; this change neither
extends that ABI nor claims OS-runtime power-off support.

## Mutable flash regions

`BoardBuildPolicy::mutable_regions` describes named prefix regions with sizes
and alignment requirements. The generic assembler validates and places them;
it does not select an Intel MRC format, enforce two 64-KiB banks, or reject
non-x86 callers. Intel's planner supplies the MRC journal geometry from its
runtime constants and the optional flash-console region. Existing Intel flash
layouts, region names, ordering and the journal wire format are unchanged.

The prefix validator checks nonzero lengths, power-of-two alignment, physical
start and length alignment, overflow and duplicate/reserved names. FFS region
capacity and space for the RO/reset container remain assembler constraints.

## Clocks and register interfaces

- FU740 PLL settings belong to the board clock plan. Unmatched supplies its
  existing 26-MHz divisors; the PRCI driver owns only the programming sequence.
  The reference frequency does not silently scale explicit settings.
- HPET access accepts a platform-selected base and reads the advertised clock
  period. Delays round up and handle low-counter wrap and long intervals.
  ICH7 owns its decoded HPET instance; Pineview and the fan controller use it.
- ICH7 uses the existing `IoApic` and `Lapic` accessors instead of another raw
  selector/window implementation. Chipset decode and routing policy stay local.

The Sunxi boot-entry ownership question remains separate: it needs a deliberate
entry/linker design review, not an automatic move based on feature names.

These changes are host-tested and cross-built, not hardware-validated. In
particular, they do not replace the outstanding X200/X201 PR-head cold/cache-hit
and S3 regression checks.
