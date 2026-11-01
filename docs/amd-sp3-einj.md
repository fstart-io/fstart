# SP3 native memory EINJ

Restricted ACPI EINJ for **correctable DRAM errors** on Rome. Linux uses the
standard EINJ interface. SMM validates each request and asks the PSP to
inject into a firmware-owned scratch cache line. Nothing is injected through
`INT 18` or by writing MCA status.

Scope limits:

- Only error type `0x8` (Memory Correctable). Processor, PCIe, uncorrectable,
  deferred, fatal and vendor errors are not advertised or accepted.
- Only firmware-owned scratch pages. No arbitrary physical address.
- Does not exercise the #MC observer; see [amd-sp3-ras.md](amd-sp3-ras.md).

## Enablement

The ACPI EINJ table is published only when every condition holds:

| # | Condition | Checked in |
|---|---|---|
| 1 | Policy `memory_einj: true` (S8030 only) | `boards/tyan/s8030/src/smm.rs`, `crates/platform-sp3/src/smm.rs` |
| 2 | Generation Rome (Milan and Naples are not enabled) | `smm.rs` (`smm_info`), `flow/mod.rs` (`plan_einj`) |
| 3 | 1 or 2 sockets (`MAX_SOCKETS = 2`) | same |
| 4 | SmmInfo registration succeeds | `smm.rs` (`smm_info`) |
| 5 | PSP action 1 (enable) succeeds | `smm/einj.rs` (`init`) |
| 6 | At least one socket's target decodes to that socket from live DF/UMC state, with UMC `DisErrInj` clear and `RdEccEn` set | `einj.rs` (`validate_target`), `psp/ras.rs` (`memory_target`) |
| 7 | `SUPPORTED` in the shared page reads 8 and the ACPI SMI port exists | `flow/handoff.rs`, `acpi/mod.rs` |

Firmware never clears `DisErrInj` (UMC `MiscCfg`, UMC base `+0x1E0`, bit 2).
If the memory-init configuration leaves injection disabled, condition 6 fails
and no table is published. Enabling the PSP does not override that gate.
`RdEccEn` is UMC `EccCtrl` (`+0x14C`, bit 10).

## ACPI interface (`crates/platform-sp3/src/acpi/einj.rs`)

Table: `EINJ`, 48-byte header plus 9 serialization instructions. Action
numbers follow ACPI 6.5 §18.6:

| Action | Instruction | Effect |
|---|---|---|
| 0 BEGIN | write `0xD3` to SMI port `0xB2` | SMM begins an operation |
| 1 GET_TRIGGER_TABLE | read `BASE+8` | Trigger table pointer |
| 2 SET_ERROR_TYPE | write `BASE+32` | Legacy type (firmware sets 0 at init) |
| 3 GET_ERROR_TYPE | read `BASE+0` | `SUPPORTED` (8) |
| 4 END | write `0xD4` to `0xB2` | Ends the operation |
| 5 EXECUTE | write `0xD5` to `0xB2` | Validates and injects |
| 6 CHECK_BUSY | read `BASE+16` | `BUSY` |
| 7 GET_COMMAND_STATUS | read `BASE+24` | `STATUS` |
| 8 SET_ERROR_TYPE_WITH_ADDRESS | write `BASE+40` | Linux writes the 36-byte parameter structure here |

Trigger table (`acpi::einj::trigger`): one `READ_REGISTER` from a memory GAS.
The address is the target line, patched by SMM at EXECUTE. Before that it is
the first owned target. The trigger is installed at `BASE+128`.

## Shared communication page (`crates/platform-sp3/src/einj.rs`)

`BASE = 0x75BF0000` (end of `STORE`, minus 64 KiB). The page is inside the
reserved early region. Each field is 8 bytes unless noted.

| Offset | Name | Writer | Meaning |
|---|---|---|---|
| `+0` | `SUPPORTED` | SMM | 8 when published, else 0 |
| `+8` | `TRIGGER_POINTER` | firmware | Address of the trigger table |
| `+16` | `BUSY` | SMM | 1 during an operation |
| `+24` | `STATUS` | SMM | 0 ok, 1 disabled or failed, 2 invalid request |
| `+32` | `LEGACY_TYPE` | OS | Written by ACPI action 2; not used for selection |
| `+40` | `PARAMETERS` | OS | 36-byte request (below) |
| `+128` | `TRIGGER` | firmware | ACPI trigger table, 48 bytes |
| `+192` | `BACKEND_STATUS` | SMM | `[0]` enable, `[1]` begin, `[2]` execute, `[3]` end |
| `+224`, `+232`, `+240` | diagnostics | SMM | Registration status, trace count, policy |
| `+256 + 8n` | trace | SMM | Up to 32 read-only trace entries |
| `+512` | `TARGETS` | SMM | Output only: suggested line per socket (2 × u64, 0 = none) |
| `+528` | `SOCKET_COUNT` | SMM | Output only |

`PARAMETERS` (`einj.rs::Parameters`, 36 bytes, packed):

| Offset | Field | Accepted value |
|---|---|---|
| 0 | `error_type` | 8 |
| 4 | `vendor_offset` | 0 |
| 8 | `flags` | 2 |
| 12 | `apic_id` | ignored |
| 16 | `address` (u64) | target; rounded down to 64 bytes |
| 24 | `mask` (u64) | `!(2^k - 1)` for `k ≤ 12` (granularity ≤ 4 KiB) |
| 32 | `pcie_sbdf` | ignored |

The request is copied once at EXECUTE. The line must lie in a page that
SMM owns (`EinjConfig.targets` in SMRAM). The shared `TARGETS` output is
never read back as authority.

`BACKEND_STATUS` values (`smm/einj.rs::status`): PSP status word, or
`0x1_0000` timeout, `0x2_0000` recovery, `0x3_0000` unsupported,
`0x4_0000` other local error.

## Target selection

- Legacy scratch page `SCRATCH = 0x75BF1000` (4 KiB) is always reserved.
- Other socket targets come from `einj::plan_targets`. For each socket it
  tries cache lines in the legacy page, then pages inside live DRAM rules
  (single-bit probes at address bits 8–16). It keeps the first line that
  decodes to that socket.
- `flow/mod.rs::plan_einj` reserves any chosen page that is not the legacy
  page in e820 and zeroes it. Only usable RAM is removed. MMIO, APOB, TSEG
  and firmware are never chosen.
- A socket with no decodable line gets 0. The hwtest fails if any socket lacks
  a target.

## Target decoding (`crates/driver-amd/src/psp/ras.rs`, feature `rome`)

`memory_target(address, sockets, df, smn)` maps a physical address to socket,
channel and chip select, using live DF and UMC registers. Accepted:

- Global CCM DRAM rule, fabric-ID masks from DF3 F1x208/F1x20C.
- Separate or socket-interleaved ranges, nonzero rule bases, DRAM-hole
  remapping, local high-rule offset.
- Channel interleave modes accepted by `channel_bits()`, plus hashed modes.
- Primary chip-select match and CS hash.

Rejected (no PSP command is sent): die interleave, unknown or ambiguous
layouts, multiple CS matches, normalized address ≥ 2^40, `DisErrInj` set,
`RdEccEn` clear.

## Request sequence (`crates/platform-sp3/src/smm/einj.rs`)

| ACPI action | SMM work | PSP action (`0x22` RasEinj) |
|---|---|---|
| SmmInfo | Registration, then enable | 1 ENABLE |
| 0 BEGIN | Clears `PARAMETERS`, starts op | 2 BEGIN |
| 5 EXECUTE | Copies request, validates target, saves target socket's NBIO regs `1d840/1d848/1d84c`, patches trigger address | 4 correctable-memory request |
| — | Linux reads the trigger address | — |
| 4 END | Ends op, restores NBIO regs on target socket | 3 END |

- At most 4 EXECUTE requests per boot, one active operation at a time.
- An invalid request is rejected by firmware. Linux then does not run END,
  so SMM sends END itself.
- A PSP status error during an active operation triggers an END attempt.
  Timeouts do not, because the PSP may still own the C2P page.

Action-4 message layout (`psp/ras.rs::CorrectableMemory`, packed, 28 bytes
including the 8-byte header):

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Command header `{size = 28, status}` |
| 8 | 4 | Action = 4 |
| 12 | 8 | Physical address |
| 20 | 4 | UMC base = `0x50000 + channel × 0x100000` |
| 24 | 2 | Kind = 1 |
| 26 | 1 | Chip select |
| 27 | 1 | Socket |

Control messages (actions 1–3) have size 12.

Any nonzero header status is an error (`psp/mailbox.rs`, Ssp variant).

## Limits

- Each PSP wait: 20 ms (`with_timeout(20_000)` µs) in SMM.
- Any PSP error (timeout, recovery, status) marks EINJ failed until reset.
  No retry. Later ACPI requests get `STATUS = 1`.
- Firmware validates; Linux drives the operation. The shared page does not
  authorize anything.

## Testing

Host tests:

```sh
cargo test -p fstart-driver-amd --features rome ras
cargo test -p fstart-platform-sp3 --features acpi,rome,smm einj
```

Coverage: exact wire layout, mailbox order `1, 2, 4, 3` with PSP error
propagation, Rome DRAM-mapping vectors (1P, 2P interleave, hashed maps,
remote permission rejection, separate ranges, hole remapping), scratch
planning, parameter validation, and ACPI table checksum and layout.

Board builds: `cargo fbuild build --board tyan-s8030 --stage smm --release`
and `cargo fbuild build --board tyan-s8030 --release`. The SMM unit is audited
as described in [amd-sp3-ras.md](amd-sp3-ras.md#testing).

Hardware: `tools/hwtest/sp3-einj.c`, disposable initramfs only. Setup and
required kernel options are in [`tools/hwtest/README.md`](../tools/hwtest/README.md).
The program:

1. Checks that FACP OEM ID is `FSTART` and mounts debugfs.
2. Reads the comm page through read-only `/dev/mem` (4 KiB at `0x75BF0000`).
   It reads no other memory.
3. Requires `available_error_type` to be exactly `0x8`.
4. For each socket, injects once through `/sys/kernel/debug/apei/einj/`,
   then requires the summed EDAC `ce_count` to increase.
5. Requires `EINVAL` for a non-scratch target (`0x75BF0000`).

Reference: [ACPI EINJ kernel docs](https://docs.kernel.org/firmware-guide/acpi/apei/einj.html).

## Hardware status

- The hwtest does not validate 2P. Its README says dual-socket hardware
  validation has not been done.
- Corrected-error injection is the only path it exercises. It does not cover
  the #MC observer, #MC-on-exit, or BMC SEL delivery.
