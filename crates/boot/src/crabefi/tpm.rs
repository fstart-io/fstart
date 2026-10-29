//! fstart's measured boot continued by CrabEFI (see `fstart-tcg`).
//!
//! CrabEFI appends to the event log fstart started, in place, so the log
//! area the ACPI `TPM2`/`TCPA` table points to holds the whole boot. A
//! TPM 2.0 is driven through fstart's driver; CrabEFI's own TIS driver
//! takes a TPM 1.2.

use crabefi::{
    PlatformEventLog, Tpm2Device, Tpm2DeviceConfig, TpmDigest, TpmError, TpmEventLogConfig,
    TpmLogFormat, TpmPcrBanks,
};
use fstart_driver_tpm::{Error, Transport, tpm20};
use fstart_tcg::Family;

/// fstart's TPM 2.0 as CrabEFI's platform TPM.
struct Tpm2 {
    transport: &'static mut dyn Transport,
    banks: TpmPcrBanks,
}

impl Tpm2Device for Tpm2 {
    fn active_pcr_banks(&self) -> TpmPcrBanks {
        self.banks
    }

    fn pcr_extend(&mut self, pcr_index: u32, digests: &[TpmDigest<'_>]) -> Result<(), TpmError> {
        let mut list: [(u16, &[u8]); 5] = [(0, &[]); 5];
        let count = digests.len().min(list.len());
        for (slot, digest) in list.iter_mut().zip(digests) {
            *slot = (digest.algorithm, digest.digest);
        }
        tpm20::pcr_extend(self.transport, pcr_index, &list[..count])
            .map_err(|_| TpmError::DeviceError)
    }

    fn submit_command(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, TpmError> {
        self.transport
            .transmit(command, response)
            .map_err(|error| match error {
                Error::Response => TpmError::BufferTooSmall,
                _ => TpmError::DeviceError,
            })
    }
}

// SAFETY: CrabEFI runs its TPM code on the boot CPU only; the transport is
// owned by this value alone.
unsafe impl Send for Tpm2 {}

static mut TPM2: Option<Tpm2> = None;

/// Take fstart's TPM and event log for CrabEFI; `None` without measured
/// boot.
pub(super) fn event_log_config() -> Option<TpmEventLogConfig<'static>> {
    let tpm = fstart_tcg::take_for_payload()?;
    let banks = TpmPcrBanks::new(tpm.banks());
    let log_area = Some(PlatformEventLog {
        area: tpm.log_area,
        used: tpm.log_used,
    });
    Some(match tpm.family {
        Family::Tpm20 => {
            // SAFETY: the boot CPU launches CrabEFI once; the slot is
            // written once and owned by CrabEFI afterwards.
            let device = unsafe {
                (*core::ptr::addr_of_mut!(TPM2)).insert(Tpm2 {
                    transport: tpm.transport,
                    banks,
                })
            };
            TpmEventLogConfig {
                existing_log: None,
                log_area,
                format: TpmLogFormat::CryptoAgile,
                tpm2_device: Tpm2DeviceConfig::Driver(device),
                tpm1_tis_base: None,
            }
        }
        Family::Tpm12 => TpmEventLogConfig {
            existing_log: None,
            log_area,
            format: TpmLogFormat::Sha1Only,
            tpm2_device: Tpm2DeviceConfig::None,
            tpm1_tis_base: Some(fstart_driver_tpm::tis::BASE as u64),
        },
    })
}
