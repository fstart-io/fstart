# SP3 RAS hardware checks

`sp3-ras.c` runs only in a disposable Linux initramfs on an authorized SP3
bench. It does not mount or write disks, inject a machine check, or corrupt
memory. It reads MSRs, sends software SMIs, issues one already-zero bank-status
write, and requests one benign BMC auxiliary-log event.

Required kernel options: `CONFIG_X86_MSR=y`, `CONFIG_X86_IOPL_IOPERM=y`, SMP,
devtmpfs and the normal AMD MCA support. Boot with `msr.allow_writes=on`.
Compile the program statically for x86-64 Linux (for example using Nix's
`pkgsStatic.stdenv.cc`), install it as `/bin/sp3-ras` in the initramfs and run
it as root after normal kernel/device initialization. A failed register check
prevents subsequent write tests; nonzero existing bank status is never cleared.

The remote S8030 bench and its disk-safe Linux 7.1 payload are documented in
`sp3/reports/HW_S8030_BENCH.md` in the adjacent analysis repository. The payload
needs the extra I/O-permission option above. Its normal configuration deliberately
omits block filesystems/MD and must not be replaced with an installed host OS.

The test verifies:

- Every online CPU has #MC-to-SMI enabled with the expected trigger.
- PFEH really is armed **inside SMM**, not merely read as zero from Linux.
- The cloak and deferred-interrupt masks are zero, preserving OS bank ownership.
- A software command alone does not manufacture an MCA cause.
- An already-zero OS MCA-status write returns, and observation remains armed.
- The dedicated KCS2 path returns a completion/record ID for a benign event.

Diagnostics use a register-only protocol described in `docs/amd-sp3-ras.md`.
Requests are paced by 1 ms to avoid treating coalesced back-to-back software
SMIs as missing firmware. The probe's elapsed time includes this pacing.

## What these checks do not prove

They do **not** prove native #MC-on-exit or hardware #MC redirection. Linux
7.1 `arch/x86/kernel/cpu/mce/inject.c` uses `INT 18` in `trigger_mce()`; software
MCE logging or writing synthetic MCA status must not be presented as proof of
a physical error traversing PFEH/SMM.

Real error injection needs a controlled, allocated target and a silicon/PSP
injection backend. ACPI EINJ is one standard interface, but fstart does not
currently expose it. See https://docs.kernel.org/firmware-guide/acpi/apei/einj.html.
The Rome donor also contains a PSP RAS-EINJ backend (C2P command 22); its action
semantics and target validation must be fully recovered before using it. There
is no need to introduce DXE to implement such a backend. Never use arbitrary
physical addresses, CPU-cache corruption or fatal injection on a host with
valuable storage or workloads.

BMC command success is not equivalent to persistent/indexed SEL delivery.
The bench's BMC silently ignores OEM E0 records, while standard records create
`xyz.openbmc_project.Logging.SEL.Error.Created` entries. Inspect those entries
and their metadata separately from the BMC's legacy Get SEL Info cache.
