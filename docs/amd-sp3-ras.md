# Native SP3 SMM RAS observer

This is an **experimental MCA observer**, not an AGESA firmware-first RAS
replacement. The S8030's normal SMM policy enables it; RAS is not a separate
board or Cargo feature:

```sh
cargo fbuild build --board tyan-s8030 --stage smm --release
# Complete image, without flashing:
cargo fbuild build --board tyan-s8030 --release
```

The board selects a compile-time `smm::ras::Policy`.
No DXE, UEFI protocols, setup variables or ReadyToBoot event are involved.
The existing installer configuration remains at most 32 bytes.

## Ownership and flow

1. The normal AMD SMM installer places and locks the image on every online
   CPU, enables the FCH, then exercises each CPU with a self-SMI.
2. `SmmHandler::enter_cpu` runs on **every entrant**, before attempting the
   shared chipset-dispatch lock. The first entry arms that CPU's #MC observer.
   It refuses unsupported save-state revisions, pre-B2 Naples, absent SMCA,
   more than 64 banks, and existing PFEH/interrupt/local-trigger ownership.
   Failure leaves the CPU OS-first; MSR readback failure rolls back the setup.
3. A real local `MceRedirSts`, not the forgeable SW-SMI command 0x80, authorizes
   capture. The handler requests native #MC on SMM exit *before* attempting
   logging, then snapshots each valid bank on the executing CPU. Address and
   syndrome validity and deferred-register support gate optional reads.
4. A nonblocking SMRAM journal retains the first 64 raw records until reset.
   It never overwrites initial evidence. Contention/fullness increments a
   drop counter and does not prevent native handoff. Logical CPU indices are
   installer indices (up to 256), not fabricated socket/DIMM identifiers.
5. The shared dispatcher can attempt one queued SEL record per SMI. It does
   not wait for other CPUs to arrive; a late producer may await a later SMI.
6. RSM delivers native #MC. The OS still sees the banks, MCA addresses and
   global status: firmware does not consume the error or decide recoverability.

The observer leaves the cloak mask and deferred-interrupt mask **zero**. It
never arms corrected/deferred SMI routing, changes thresholds, clears banks
on its own, recloaks banks, or claims PCIe/AER ownership. Corrected/deferred
handling, poison recovery, panic/reset decisions and ordinary bank clearing
remain OS responsibilities. A hardware fatal/reset path can bypass SMM.

### Intercepted OS writes

PFEH can cause a WRMSR SMI even with banks visible. Such an entry must not be
silently discarded. The hardware local WRMSR status authorizes reading saved
RCX/RDX/RAX; arbitrary software SMIs cannot issue MSRs through this path.

Only zero-valued OS writes to implemented MCA status registers (legacy or
SMCA, supported DESTAT, or MCG_STATUS) are replayed in SMM. HWCR permissions
are not elevated. For any other write, observation is permanently disabled
on that CPU and saved RIP is rewound over WRMSR's two-byte opcode, so the
original request executes in normal mode with native privilege/fault behavior.
With simultaneous #MC, the #MC handoff happens first; the retried WRMSR can
subsequently take the opt-out path. No arbitrary MSR write is attempted in SMM.

This fallback, native #MC-on-exit, and the interaction with OS MCA setup are
**experimental**; distinguish the hardware checks below from validation of
a real hardware-originated machine check. There is no S3 rearming implementation.

## BMC transport and limits

The S8030 policy uses firmware KCS2 at **CA8/CAC**, inside the existing
CA0..CAF LPC decode. The OS interface remains KCS3 at **CA2/CA3**. Nothing
in this path uses or aborts an in-progress OS KCS3 transaction.

- One transaction per SMI, at most eight error reports and one explicitly
  requested benign transport probe per boot.
- A transaction-wide 2,000-readiness-check budget: at most 20 ms of polling
  delays, even for a BMC that streams a response forever. This is not a
  fresh timeout per byte, nor a hard wall-clock bound on all platform I/O.
- First transport/completion failure disables further attempts until reset.
  No retry, Get SEL Info, readiness wait or KCS abort/recovery loop.
- The journal is independent of delivery. BMC failure never erases it.

The bench BMC silently acknowledges OEM non-timestamped E0 records with ID
zero without creating a log entry. The observer therefore sends standard
system-event records: BIOS generator 0001, event revision 4, Processor type
07, sensor number FF (no particular SDR sensor), sensor-specific event type
6F. Event data is `{AB, cpu, bank}` for uncorrected/deferred errors or
`{AC, cpu, bank}` for corrected errors. Bytes 2/3 are explicitly OEM data,
not invented DIMM identifiers. The full raw status stays in the journal.

BMC standard-record acceptance is distinct from how its UI/SEL cache indexes
an unspecified sensor. Inspect the BMC logging entry's additional data;
completion success alone does not establish a correctly indexed IPMI SEL.
These compact records are not CPER or address-to-DIMM attribution. Full
address/IPID/syndrome/MISC/deferred/global-status records and drop/refusal
counters remain private to locked SMRAM; there is no OS export of the journal.

### Bounded diagnostics

SW SMI D1 is a pointer-free diagnostic interface. Only the interrupted context
carrying `RAX=46535241530000D1`, `RDX=B2` is modified; RCX selects:

- 0: return SMM-visible PFEH/cloak/deferred-mask in RAX/RCX/RDX.
- 1: request one benign KCS2 "entry added to auxiliary log" event, OEM data
  `FS`. This is not a manufactured machine check and does not populate the
  MCA journal. It can be requested only once per boot.
- 2: read probe state in RAX: 0 idle, 1 queued, 4 running, 2 success, 3 failure.
- 3: check saved-SMBASE placement without exposing addresses: RAX is the FF00
  match, RCX the FE00 match, RDX the save-state revision.

The client is `tools/hwtest/sp3-ras.c` (Linux initramfs only). It grants port
access, pins to each online CPU and spaces diagnostic requests by 1 ms; it
never passes an OS buffer into SMM. Normal-mode PFEH reads are insufficient:
on the Rome bench they read zero while SMM reads the armed value 1.
Ordinary software SMI 80 still does not authorize MCA capture or #MC handoff.

## Implementation boundaries

- `driver-zen/src/ccx/ras.rs`: typed per-CPU MSRs, arming, bounded capture,
  native handoff, restricted OS-clear replay and disarming.
- `smm/src/{handler,stage}.rs`: generic per-CPU entry hook; shared dispatch
  still has a single owner, with no all-CPU arrival requirement.
- `smm/src/save_state.rs`: validated AMD64 saved GPR access; native Zen
  revision 30064 and QEMU's 20064. Both use the FF00 SMBASE field;
  FE00 is the save-state block's base, as confirmed on the Rome bench.
- `platform-sp3/src/smm/ras.rs`: journal, per-CPU state and bounded reporting.
- `boards/tyan/s8030/src/smm.rs`: board policy and dedicated BMC endpoint.
- `driver-ipmi`: OEM SEL framing and transaction-wide KCS budgets.

No EINJ, GHES/SCI/NMI tables, ERST, BERT, flash error store, APCB repair queue,
DIMM decoding or PCIe firmware-first handling is implemented here. These are
separate ownership/persistence choices, not prerequisites for observing #MC.

## Evidence and verification

Register facts were independently expressed from AMD #55570 (MSRC001_0022,
MSRC001_0056, SMMxFEC4, SMMxFEFC and the AMD64 saved GPRs); the named PFEH exit
bits are also defined by OpenSIL commit
`cd8edaee7d42bc93dd4110981d15d96684271205`, `xUSL/Include/MsrReg.h`.
No reference implementation code was copied.

The SMM donor anchors are Rome S8030 V4.04 `AmdRasSspSmm`, GUID
`ee8175ef-4c60-82af-f5e2-8a9ef1fc0a9a`, executable SHA-256 prefix
`cca68f7070ef`, VAs 0x6B0 (redirection), 0x934 (exit request), and 0xAFC
(saved-register WRMSR replay), instruction-checked. The observer deliberately
omits its cloaking/rearming policy. The saved S8030 BMC flash has SHA-256
`2b09b92ee677b5d9d44ed150d2a225b64e8058ad03d1d34ad649e672399c42c3`;
DTB 0x39A200 and enabled KCS bridge services establish the two configured
interfaces, not successful runtime delivery.

Focused host tests cover arming/refusal/rollback, native handoff despite
missing/dropped evidence, capture without bank clearing, restricted write
replay, native save-state offsets, per-CPU entry before the shared lock,
journal contention/overflow, SEL encoding, diagnostic register isolation and
transaction-wide timeouts.
The real SMM image must also pass fbuild's PIC/relocation/panic audit; a host
`cargo check` alone is insufficient. No hardware access or error injection
is part of these host-only checks.

## Rome hardware results — 2026-10-26

Three flash/boot cycles were run on the authorized S8030GM2NE-NFX bench,
EPYC 7502P, 64 threads, 128 GiB, using a Linux 7.1 initramfs without block
filesystems. Only the lower 16 MiB of host flash was written and each write
was verified by readback. The bench used its **existing** PCIE#3
x4x4x4x4 wiring variant; no RAS board variant or feature is needed.

Logs are local to the adjacent analysis repository:
`firmware/hw/s8030gm2ne-CCSF2AK45016/fstart-boot-ras-native{,2,3}-20261026.log`.

- First run: normal-mode PFEH reads were zero, so the initial test refused
  to do write tests. This was an invalid test assumption, not evidence of
  failed arming. Read-only SMM-side diagnostics were added.
- Second run: all 64 CPUs reported PFEH=1 inside SMM, with both masks zero,
  despite normal-mode reads of zero. Rapid subsequent software requests
  were not reliably observed. The client now paces requests by 1 ms, and
  the FCH EOS write is read back before RSM. These changes were tested
  together, not isolated as separate causal experiments.
- Third run: all 64 CPUs were armed; software command 80 returned without
  manufacturing #MC; a zero write to already-zero bank 0 status returned;
  CPU 0 remained armed after both. Bank counts varied by thread (17/24/28).
  This does not prove that the zero write actually generated a WRMSR SMI.
- SMM-side layout check: `FF00_match=1 FE00_match=0 revision=30064`. This
  corrected the native saved-SMBASE writer: FE00 is the block base, FF00
  the field. The correction is dead-stripped from the permanent handler;
  the final rebuilt SMM image is byte-identical to the hardware-tested one.
- All three runs booted Linux, enumerated the bench hardware, and powered
  off. The final run had all 64 CPUs online, six NVMe controllers, EDAC
  `ce=0 ue=0`, no kernel panic/Oops/BUG, and normal ACPI poweroff.

**One check still fails:** the real SMM KCS2 probe returned diagnostic state
3, elapsed 32,096 us including client pacing. Its request nevertheless
reached the BMC: logging entry 45 and serialized file
`/var/lib/phosphor-logging/errors/45` exist, with
`RECORD_TYPE=2`, `GENERATOR_ID=1`, `SENSOR_DATA=A34653`, `SENSOR_PATH=""`.
Thus transport submission/storage were observed, but successful completion
within the firmware budget was not. The final hardware helper correctly
exits with one failure; this is not an all-green end-to-end RAS test. The
BMC's legacy Get SEL Info cache did not reflect these unspecified-sensor
logging entries. OEM E0 tests returned ID zero and created no logging entry.

No actual machine check, memory corruption or fatal error was injected.
Native #MC redirection, #MC-on-exit, the intercepted-write retry fallback,
Milan and 2P remain unvalidated on hardware. See `tools/hwtest/README.md`
for the distinction from Linux software injection and the real-injection
backend work still required.

Final tested SMM SHA-256:
`61999d5cecfb9d6be6a8f670e25b5288cc4e0144410789a785f0de240e1ee694`.
Final test image lower-half SHA-256:
`4d7f340d9be7c0c39a333d35cdada84729a05955a81855b0be618cc41dc8afd9`.

The **pre-test firmware was restored**, not left running this prototype.
Its lower-half SHA-256 was verified after restoration:
`55ee3e777f6e7bbb47d18873c64ff7d0ab4a5657489b51b4e7adc9067ec2401c`.
The host was left off, SPI unbound from the BMC and the GPIO holder released;
the bench lease was released. No host data disks were written.
