# Native SP3 SMM RAS observer

This is an **experimental MCA observer**, not an AGESA firmware-first RAS
replacement. It is disabled in ordinary board builds. The S8030 opt-in is:

```sh
cargo fbuild build --board tyan-s8030-ras --stage smm --release
# Complete image, without flashing:
cargo fbuild build --board tyan-s8030-ras --release
```

The board's `ras-observer` feature selects a compile-time `smm::ras::Policy`.
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
**not hardware-validated**. They are reasons this remains opt-in. There is no
S3 rearming implementation.

## BMC transport and limits

The S8030 policy uses firmware KCS2 at **CA8/CAC**, inside the existing
CA0..CAF LPC decode. The OS interface remains KCS3 at **CA2/CA3**. Nothing
in this path uses or aborts an in-progress OS KCS3 transaction.

- One transaction per SMI, at most eight attempts per boot.
- A transaction-wide 200-readiness-check budget: at most 2 ms of polling
  delays, even for a BMC that streams a response forever. This is not a
  fresh timeout per byte, nor a hard wall-clock bound on all platform I/O.
- First transport/completion failure disables further attempts until reset.
  No retry, Get SEL Info, readiness wait or KCS abort/recovery loop.
- The journal is independent of delivery. BMC failure never erases it.

Records use the IPMI OEM non-timestamped type E0, not a guessed SDR sensor:

| Bytes | Encoding |
|---|---|
| 0..1 | Record ID, zero on submission |
| 2 | E0 |
| 3..4 | ASCII `FS` |
| 5..6 | Logical CPU index, u16 little-endian |
| 7 | Bank index |
| 8..15 | Raw MCA_STATUS, u64 little-endian |

BMC acceptance of this OEM payload is untested. It is a compact diagnostic,
not CPER, address-to-DIMM attribution, a timestamped record, or a promise of
nonvolatile delivery. Full address/IPID/syndrome/MISC/deferred/global-status
records and drop/refusal counters remain private to locked SMRAM; there is
currently no OS export/management interface for the journal.

## Implementation boundaries

- `driver-zen/src/ccx/ras.rs`: typed per-CPU MSRs, arming, bounded capture,
  native handoff, restricted OS-clear replay and disarming.
- `smm/src/{handler,stage}.rs`: generic per-CPU entry hook; shared dispatch
  still has a single owner, with no all-CPU arrival requirement.
- `smm/src/save_state.rs`: validated AMD64 saved GPR access; native Zen
  revision 30064 and its FE00 SMBASE field, distinct from QEMU's 20064/FF00.
- `platform-sp3/src/smm/ras.rs`: journal, per-CPU state and bounded reporting.
- `boards/tyan/s8030/src/smm.rs`: opt-in policy and dedicated BMC endpoint.
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
journal contention/overflow, SEL encoding and transaction-wide timeouts.
The real SMM image must also pass fbuild's PIC/relocation/panic audit; a host
`cargo check` alone is insufficient. No hardware access or error injection
is part of these checks.
