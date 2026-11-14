// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measured boot on a discrete TPM, for every SoC
//! ([`crate::board::Board::DISCRETE_TPM`], `fstart-tcg`).
//!
//! The reset image is the static root of trust: the PSP copies it from the
//! booted flash (BIOS directory entry `0x62`) and releases x86 into it, so
//! it measures itself (PFP §3.3.4.1, no H-CRTM): its version and the flash
//! copy the PSP loaded, then the SMM handler it installs
//! ([`crate::tseg::install`]). Payload launchers measure the rest. The TCG
//! event log lives in the reserved early region and ACPI points the OS at
//! it (`TPM2`/`TCPA`).

use fstart_driver_tpm::tis::{self, Tis};
use fstart_tcg::event::EV_S_CRTM_CONTENTS;

/// The event log area (LAML), as EDK2 sizes it.
const LOG_SIZE: usize = 0x1_0000;

static mut TIS: Option<Tis<fstart_arch::x86::Udelay>> = None;

/// Start measured boot on the TPM, if one answers, and measure the flash
/// copy of the reset image the PSP loaded, `image`. `server`: the TCG
/// platform class of the event log.
pub fn start(image: Option<&'static [u8]>, server: bool) -> Result<(), &'static str> {
    // SAFETY: the FCH decodes FED4_0xxx to the TPM on LPC (D14F3x7C
    // Tpm12En, set at reset); nothing else drives it.
    let tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    let Some(id) = tis.probe() else {
        fstart_log::info!("tpm: no TPM at {:#x}", tis::BASE);
        return Ok(());
    };
    if tis.is_crb() {
        fstart_log::warn!("tpm: CRB TPM at {:#x} not supported, left alone", tis::BASE);
        return Ok(());
    }
    fstart_log::info!(
        "tpm: {:#x}:{:#x} revision {} at {:#x}",
        id.vendor,
        id.device,
        id.revision,
        tis::BASE
    );
    // SAFETY: the boot CPU runs this once; the slot is written once.
    let tis = unsafe { (*core::ptr::addr_of_mut!(TIS)).insert(tis) };
    let log = crate::sp3::silicon::steps::dma_alloc(LOG_SIZE, 0x1000)?;
    let uefi = fstart_payload::SELECTED == fstart_payload::Selected::Uefi;
    let class = if server {
        fstart_tcg::PlatformClass::Server
    } else {
        fstart_tcg::PlatformClass::Client
    };
    if let Err(error) = fstart_tcg::start(tis, log, class, uefi) {
        error.report();
        match error.remedy() {
            fstart_tcg::Remedy::Halt => return Err("TPM protection"),
            // A reset would fail the same way; hidden instead (no ACPI).
            fstart_tcg::Remedy::Reset => fstart_log::error!("tpm: wrong startup locality, hidden"),
            fstart_tcg::Remedy::Hide | fstart_tcg::Remedy::None => {}
        }
        return Ok(());
    }
    fstart_tcg::measure_crtm_version(fstart_stage::tcg::CRTM_VERSION);
    // The PSP copied the stage to DRAM, where it already ran and changed its
    // data: the root of trust measures the image where the flash maps it.
    match image.map(fstart_stage::boot::InPlace::mapped) {
        Some(image) => {
            let event = fstart_stage::boot::Event {
                pcr: 0,
                kind: EV_S_CRTM_CONTENTS,
                description: "fstart reset image",
                base: image.address(),
            };
            image.measure(&fstart_stage::boot::Measurement {
                log: &fstart_tcg::Tcg,
                event,
            });
        }
        None => {
            fstart_log::error!("tpm: no reset image entry to measure");
            fstart_tcg::measurement_error();
        }
    }
    Ok(())
}

/// The SMM handler, before it is installed.
pub fn measure_smm(image: &'static [u8]) {
    use fstart_tcg::event::{EV_POST_CODE2, post_code};
    fstart_tcg::measure_blob(
        0,
        EV_POST_CODE2,
        post_code::SMM_CODE,
        image.as_ptr() as u64,
        image,
    );
}

/// The ACPI description of the TPM measured boot started on. A TPM that
/// could not start, also an owner-disabled TPM 1.2, is not described.
pub fn acpi() -> Option<fstart_acpi::devices::tpm::TpmTisAcpi> {
    use fstart_acpi::devices::tpm::{LogArea, TpmFamily, TpmTisAcpi};
    let log = fstart_tcg::log()?;
    fstart_log::info!("acpi: TPM event log at {:#x}", log.base);
    Some(TpmTisAcpi {
        family: match log.family {
            fstart_tcg::Family::Tpm12 => TpmFamily::Tpm12,
            fstart_tcg::Family::Tpm20 => TpmFamily::Tpm20,
        },
        server: true,
        log: Some(LogArea {
            base: log.base,
            size: log.capacity as u32,
        }),
    })
}
