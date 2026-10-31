# Measured boot

fstart uses a TPM to record boot measurements in PCRs and a TCG event log.
This records what ran; it does not replace [signature verification](authenticated-boot.md).

TPM 1.2 uses SHA-1 and a legacy event log. TPM 2.0 uses a crypto-agile
log and extends every allocated PCR bank. Both use the TIS/FIFO interface.

## Measurements

The first fstart stage is the static root of trust and measures itself.

| PCR | Measured by fstart |
| --- | --- |
| 0 | Firmware version, initial stage, SMM handler, coreboot payload and its files |
| 4 | Kernel when booting Linux directly |
| 9 | Command line when booting Linux directly |

FFS files are measured as stored in flash, before decompression. fstart
does not measure configuration into PCR 1–3. Separators in PCR 0–7 mark
the end of firmware measurements; who writes them depends on the payload.

## Payload interface

There is one event log for the whole boot. The ACPI `TPM2` or `TCPA` table
publishes its physical address (LASA) and capacity (LAML). Payloads must
preserve existing entries and append in place, without restarting the TPM
or resetting the measurement chain.

| Payload | Log handoff | Writes separators in PCR 0–7 | Locks the TPM |
| --- | --- | --- | --- |
| Coreboot payload | ACPI `TPM2`/`TCPA` table | Payload | Payload |
| CrabEFI | `TpmEventLogConfig::log_area` | CrabEFI at ReadyToBoot | fstart before handoff |
| Direct Linux | ACPI `TPM2`/`TCPA` table | fstart | fstart |

- **Coreboot payloads:** receive a running TPM and an unfinished log.
  SeaBIOS requires the fstart fork's event-log continuation support.
- **CrabEFI:** receives the log buffer and used length as `PlatformEventLog`.
  TPM 2.0 commands use a supplied `Tpm2Device` driver and active-bank list;
  TPM 1.2 uses CrabEFI's TIS driver at the supplied base address.
- **Direct Linux:** receives a completed firmware log and accesses the TPM
  through its own driver. There is no firmware TPM service to call.

Locking protects platform control, not PCR extension: TPM 2.0 uses a random
platform authorization, or disables the platform hierarchy if that fails;
TPM 1.2 deasserts and locks physical presence.

## Availability and failures

Measured boot is supported on QEMU q35 with `tpm-tis` and on SP3 boards
with `Board::DISCRETE_TPM`. Without a usable TPM, boot can proceed without
measurements. A measurement failure stops further measurements and caps
PCR 0–7 so the incomplete chain cannot appear valid. fstart refuses payload
handoff if required locking or failure capping cannot be completed.

CRB, physical-presence requests (PPI), memory overwrite requests (MOR),
and S3 TPM state restoration are not supported.
