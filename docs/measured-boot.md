# Measured boot (TCG, TPM 1.2 and 2.0)

fstart measures the boot into a discrete TPM and leaves one TCG event log
for its payload and the OS. TPM 2.0 follows the TCG PC Client Platform
Firmware Profile 1.06 (crypto-agile log, every allocated PCR bank); TPM 1.2
the TCG EFI Platform 1.22 and Conventional BIOS 1.21 specifications (SHA-1
log).

## Components

| Path | Contents |
| --- | --- |
| `crates/driver-tpm` | FIFO interface of TIS 1.3 / PTP (MMIO, tock-registers); the TPM 1.2 and 2.0 commands firmware sends |
| `crates/tcg` | Startup (PFP §3.3.2.1), measurements, event log, separators, locking, failure capping |
| `crates/crypto/src/pcr.rs` | SHA-1/256/384/512 by TPM algorithm ID |
| `crates/stage/src/tcg.rs` | Measuring FFS files; payload handoff |
| `crates/boot/src/crabefi/tpm.rs` | Hands the TPM and the log to CrabEFI |
| `crates/acpi/src/devices/tpm.rs` | `MSFT0101`/`PNP0C31` device, `TPM2` and `TCPA` tables with the log area |
| `crates/platform-qemu`, `crates/platform-sp3` | Platform bring-up (below) |

## What is measured

The first fstart stage is the static root of trust and measures itself
(no H-CRTM):

| PCR | Event | What |
| --- | --- | --- |
| 0 | `EV_NO_ACTION` | Spec ID event(s), not extended |
| 0 | `EV_S_CRTM_VERSION` | `fstart <version>` |
| 0 | `EV_S_CRTM_CONTENTS` | The running stage: its FFS file (q35), the PSP-loaded reset image (SP3) |
| 0 | `EV_POST_CODE2` "SMM CODE" | The SMM handler |
| 0 | `EV_POST_CODE2` "POST CODE" | A coreboot payload (SeaBIOS) and its files |
| 0–7 | `EV_SEPARATOR` | End of the pre-OS firmware (by fstart or the payload, below) |
| 4 | `EV_EFI_PLATFORM_FIRMWARE_BLOB2` | Direct Linux boot: the kernel |
| 9 | `EV_EVENT_TAG` | Direct Linux boot: the command line, as Linux's EFI stub records its load options |

TPM 1.2 logs use the TPM 1.2 event types (`EV_POST_CODE`,
`EV_EFI_PLATFORM_FIRMWARE_BLOB`). FFS files are measured as stored in
flash: their SHA-256 digest equals the one in the authenticated FFS
directory. fstart measures no configuration into PCR 1–3 yet.

## One log for the whole boot

The log lives where the ACPI `TPM2` or `TCPA` table (LAML/LASA) points,
and every later phase appends to it in place:

| Payload | Separators | TPM lock |
| --- | --- | --- |
| coreboot payload (SeaBIOS) | the payload | the payload, as on coreboot |
| CrabEFI | CrabEFI, at ReadyToBoot | fstart, before CrabEFI |
| Linux (direct) | fstart | fstart |

Locking means a random platform authorization (TPM 2.0, PFP §11), or the
platform hierarchy disabled if that fails, and physical presence
deasserted and locked on TPM 1.2. If the TPM cannot be locked, or PCR 0–7
could not be capped after a failure, fstart halts instead of starting the
payload.

SeaBIOS needs the `fstart-tpm` change of the fstart SeaBIOS fork to
continue the log; CrabEFI takes it through `TpmEventLogConfig::log_area`.

## Platforms

- **q35**: with a TPM, QEMU's own `TPM2`/`TCPA` table points at
  `etc/tpm/log` in the ACPI buffer, which becomes the log. Only `tpm-tis`
  works (no CRB).
- **SP3**: boards set `Board::DISCRETE_TPM`. The reset image probes the
  TIS at `FED4_0000h` (LPC), starts measured boot at POST 16 and describes
  the TPM in ACPI (server class). A TPM that cannot be used is not
  described. Tested on the Tyan S8030 with an Infineon SLB9665 (TPM 2.0).

## Failures

A TPM that does not start is left alone. A started TPM that cannot be
measured into (self test, banks, log) is hidden: TPM 2.0 gets its
endorsement and storage hierarchies disabled, PCR 0–7 capped and the
platform authorization randomized; TPM 1.2 gets physical presence locked.
A measurement failure later caps PCR 0–7 (`00000001h`) and stops
measuring. Not implemented: PPI and MOR, CRB, S3 `TPM2_Startup(STATE)`.

## Testing on QEMU

```sh
# Linux guest images (needs grub2, xorriso, mtools, gptfdisk, dosfstools)
ci/tpm/build-guest.sh target/tpm-guest KERNEL BUSYBOX GRUB_EFI_MKSTANDALONE
# SeaBIOS with the fstart-tpm change
ci/payloads/seabios/build.sh SEABIOS_SRC target/seabios
# TPM 1.2 and 2.0 with SeaBIOS and CrabEFI (needs swtpm)
ci/tpm/run-tests.sh target/tpm-guest target/seabios
```

Each run boots Linux, which prints its PCRs and the event log.
`ci/tpm/verify-log.py` requires PCR 0–9 for every requested bank and compares
all of them with a replay. The log must contain the firmware and payload
measurements and successful separators for PCR 0–7 (zero, or the conventional
BIOS `ffffffffh` used by SeaBIOS); error separators, missing banks/readings
and truncated logs fail the test.

The `tpm-boot-tests` CI job runs all five combinations above, using a pinned
SeaBIOS revision with event-log continuation. It is part of the required
`qemu-boot-tests` aggregate, alongside the ordinary no-TPM boot matrix.
Verifier regressions run with `python3 -m unittest discover -s ci/tpm`;
`cargo test -p fstart-tcg` also covers log exhaustion, measurement failure,
failed capping and failed locking. These tests do not replace hardware cold
boot, warm reboot and absent-TPM testing.

A single boot:
`ci/tpm/qemu-tpm.sh 2.0 -- --board qemu-q35 --payload uefi --disk ...`.
