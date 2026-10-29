# SP3 SMM MCA observer

Experimental. The S8030 SMM policy observes machine checks (#MC) in SMM and
hands every one to the OS. The OS keeps MCA banks, corrected/deferred
interrupts, thresholds and recovery. Firmware does not consume errors or
decide recoverability. This is not an AGESA firmware-first RAS replacement:
there is no GHES, ERST, BERT or DXE/UEFI involvement.

Related: [native EINJ](amd-sp3-einj.md), [SP3 overview](amd-sp3.md),
[SMM](smm.md).

## Enablement

- No separate board or Cargo feature. `boards/tyan/s8030/src/smm.rs` binds
  `Sp3SmmHandler<Ras>` with `OBSERVE_MCE = true`.
- Other boards use the default `OsFirst` policy (`crates/platform-sp3/src/smm/ras.rs`),
  which arms nothing.

```sh
cargo fbuild build --board tyan-s8030 --stage smm --release   # SMM unit only
cargo fbuild build --board tyan-s8030 --release               # complete image
```

Policy (`crates/platform-sp3/src/smm/ras.rs`):

| Item | Meaning |
|---|---|
| `OBSERVE_MCE` | Arm the per-CPU #MC observer |
| `MEMORY_EINJ` | Enable the [restricted EINJ](amd-sp3-einj.md) (S8030 sets it too) |
| `SEL: Option<SelTransport>` | Firmware-exclusive KCS endpoint for SEL delivery; S8030 uses port `0xca8`, spacing 4 |

## Runtime flow

| Step | Code | Behavior |
|---|---|---|
| 1. Install | `crates/platform-sp3/src/flow/mod.rs` (`install_smm`) | Handler image locked in SMRAM on every thread, one self-SMI per CPU, then SmmInfo via SW SMI `0xD0` |
| 2. Entry | `crates/smm/src/stage.rs` → `Sp3SmmHandler::enter_cpu` → `smm::ras::enter` | Runs on every SMI on every CPU, before the rendezvous. Nothing waits for other CPUs |
| 3. Arm | `sp3/ccx/ras.rs::arm` | First SMI on each CPU only. Sets state to armed or refused; refused CPUs stay OS-first |
| 4. Capture | `ras::observe` | Only when the local SMI status (save state `FEC4`, bit 8 `MCE_REDIRECT`) is set. Sets `PFEH_CONFIG.MCE_ON_EXIT`, then records each valid bank |
| 5. Journal | `smm/ras.rs` (`Journal`) | Locked SMRAM, first 64 records kept until reset. Overflow and lock contention increment a drop counter and never block |
| 6. Delivery | `smm/ras.rs::flush`, called from `handle` before dispatch | At most one SEL record per SMI (see below) |
| 7. Exit | hardware | RSM delivers the native #MC. Banks and `MCG_STATUS` are unchanged |

### Arm refusals

`ArmError`, checked before any write:

| Refusal | Condition |
|---|---|
| `UnsupportedCpu` | Naples stepping < 2 (pre-B2) |
| `UnsupportedBanks` | `MCG_CAP.COUNT` is 0 or > 64, or `MCA_CONFIG.MCAX` clear |
| `AlreadyOwned` | `MCE_REDIRECT` vector/SMI enable, `SMI_IO_CYCLE` enable, PFEH enable, `PFEH_CLOAK` or deferred mask already nonzero |
| `Readback` | Written values don't read back; the three MSRs are rolled back |

Also refused: save-state revision not accepted by the AMD64 format (`0x20064`
or `0x30064`, `crates/smm/src/save_state.rs`).

### What the observer leaves alone

- Bank registers, `MCG_STATUS`, thresholds, bank cloaking and the deferred
  interrupt mask. Cloak and deferred mask stay zero.
- Corrected/deferred SMI routing, AER/PCIe ownership, poison recovery, and
  panic or reset decisions.
- Journal export. Records are private to locked SMRAM; nothing reads them
  except the SEL path and tests. Drop and refused counters are internal.
- Re-arming: state is per CPU in SMM statics, tried once on that CPU's first
  SMI. There is no re-arm path after an OS-visible MSR reset.

Local status is inspected on every SMI of an armed CPU. A #MC SMI is the only
case that captures; software SMI `0x80` is a no-op in dispatch and never
authorizes capture.

## Intercepted OS WRMSR

Local status bit 11 (`WRMSR`) means PFEH intercepted an OS write. The saved
`RCX`/`RAX`/`RDX` are replayed only if all of these hold
(`ras::replay_clear`):

- The value is zero.
- The index is `MCG_STATUS`, a legacy `MCi_STATUS` (`0x401 + 4*i`), an SMCA
  `MCi_STATUS` (`0xC0002001 + 16*i`), or a `MCi_DESTAT` (`0xC0002008 + 16*i`)
  when `DeferredLoggingSupported` is set.
- The bank index is implemented (`< min(MCG_CAP.COUNT, 64)`).

Otherwise:

- If no #MC redirect is pending in the same SMI, the CPU is disarmed
  permanently (`ras::disarm`) and the saved RIP is rewound by 2 bytes, so the
  original WRMSR re-executes natively after RSM.
- If a #MC redirect is also pending, the #MC is handed off first. The rewound
  WRMSR then traps again with no redirect pending and disarms.

No arbitrary MSR write is performed in SMM.

## BMC transport and SEL

| Item | Value |
|---|---|
| Firmware KCS2 (S8030) | `0xCA8`/`0xCAC`, spacing 4 |
| OS KCS3 | `0xCA2`/`0xCA3` (asserted in `boards/tyan/s8030/src/bmc.rs`). Not used or aborted by this path |
| Transaction budget | `SEL_POLL_BUDGET = 2000` readiness checks, each failed check delays 10 µs (about 20 ms max delay). Whole request, not per byte (`driver-ipmi/src/kcs.rs`) |
| Attempts per SMI | one transaction |
| Attempts per boot | 8 journal records, plus at most one probe |
| Failure | first failed transaction sets `SEL_FAILED`; no further sends until reset. No retry, Get SEL Info or KCS recovery |

Standard system-event records (`smm/ras.rs::event`), not OEM records:

| Field | MCA record | Probe record |
|---|---|---|
| Record type | `0x02` system event | `0x02` |
| Generator ID | `0x0001` | `0x0001` |
| Event message revision | `0x04` | `0x04` |
| Sensor type | `0x07` (processor) | `0x12` (system event) |
| Sensor number | `0xFF` (no SDR sensor) | `0xFF` |
| Event dir/type | `0x6F` | `0x6F` |
| Event data 1 | `0xAB` uncorrected or deferred; `0xAC` corrected | `0xA3` |
| Event data 2 | logical CPU index (installer index, up to 256) | `'F'` |
| Event data 3 | MCA bank | `'S'` |

Data bytes 2/3 are OEM data. They are not DIMM, socket or address
attribution. Full raw status, address, IPID, syndrome, MISC and deferred
registers exist only in the SMRAM journal.

## Diagnostic SW SMI `0xD1`

Fixed register protocol, `OUT 0xB2, AL=0xD1` with the interrupted context
`RAX = 0x46535241530000D1`, `RDX = 0xB2`, `RCX = selector`. Only that context
is written. No pointers, error injection or MSR writes. Armed CPUs only.

| `RCX` | Returns |
|---|---|
| 0 | `RAX` = PFEH_CONFIG, `RCX` = PFEH_CLOAK, `RDX` = deferred mask |
| 1 | Requests the one benign probe (once per boot). Probe event is `0xA3 'F' 'S'` |
| 2 | `RAX` = probe state: 0 idle, 1 queued, 4 running, 2 success, 3 failure |
| 3 | `RAX` = 1 if FF00 holds SMBASE, `RCX` = 1 if FE00 holds SMBASE, `RDX` = save-state revision |

Save-state layout (`crates/smm/src/save_state.rs`): SMBASE field at FF00
(AMD64 format), FE00 is the save-state block base.

## Testing

Host tests:

```sh
cargo test -p fstart-driver-amd --features rome ras
cargo test -p fstart-platform-sp3 --features acpi,rome,smm ras
```

Covers arm/refusal/rollback, bank capture without clears, WRMSR replay rules,
journal ordering and overflow, SEL encoding, diagnostic isolation.

Image audit: building the `smm` unit runs `audit_smm_blob`
(`crates/image-build/src/smm_image.rs`). It rejects GOT indirects,
non-relative relocations, undefined symbols and panic symbols.

Hardware: `tools/hwtest/sp3-ras.c`, a static initramfs program. Prerequisites
and run instructions are in [`tools/hwtest/README.md`](../tools/hwtest/README.md).
It checks, on each online CPU, that PFEH is armed inside SMM and that the
cloak and deferred masks are zero. It then does one software SMI `0x80`, one
already-zero write to bank 0 status, and one SEL probe. It does not inject
errors.

It does **not** exercise native #MC redirection, #MC-on-exit, the WRMSR
fallback or SEL delivery of MCA records. Software injection (`INT 18`,
synthetic MCA status writes) is not a substitute.
