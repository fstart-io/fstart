//! Measured boot on a discrete TPM ([`Board::DISCRETE_TPM`], `fstart-tcg`).
//!
//! The reset image is the static root of trust: the PSP copies it from the
//! booted flash half (BIOS directory entry `0x62`) and releases x86 into
//! it, so it measures itself (PFP §3.3.4.1, no H-CRTM): its version and the
//! flash copy the PSP loaded, then the SMM handler it installs. Payload
//! launchers measure the rest. The TCG event log lives in the reserved
//! early region and ACPI points the OS at it (`TPM2`/`TCPA`).
//!
//! [`Board::DISCRETE_TPM`]: crate::board::Board::DISCRETE_TPM

use fstart_amdfw_format::directory::bios;
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_tpm::tis::{self, Tis};
use fstart_tcg::event::{EV_POST_CODE2, EV_S_CRTM_CONTENTS, post_code};

/// The event log area (LAML), as EDK2 sizes it.
const LOG_SIZE: usize = 0x1_0000;

static mut TIS: Option<Tis<fstart_arch::x86::Udelay>> = None;

/// Start measured boot on the TPM, if one answers, and measure the reset
/// image of the booted half `flash`.
pub(super) fn start(flash: &'static [u8], generation: Generation) {
    // SAFETY: the FCH decodes FED4_0xxx to the TPM on LPC (D14F3x7C
    // Tpm12En, set at reset); nothing else drives it.
    let tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    let Some(id) = tis.probe() else {
        fstart_log::info!("tpm: no TPM at {:#x}", tis::BASE);
        return;
    };
    if tis.is_crb() {
        fstart_log::warn!("tpm: CRB TPM at {:#x} not supported, left alone", tis::BASE);
        return;
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
    let log = super::dma_alloc(LOG_SIZE, 0x1000);
    let uefi = cfg!(fstart_payload = "crabefi");
    if let Err(error) = fstart_tcg::start(tis, log, fstart_tcg::PlatformClass::Server, uefi) {
        error.report();
        match error.remedy() {
            fstart_tcg::Remedy::Halt => super::fail("TPM protection"),
            // A reset would fail the same way; hidden instead (no ACPI).
            fstart_tcg::Remedy::Reset => fstart_log::error!("tpm: wrong startup locality, hidden"),
            fstart_tcg::Remedy::Hide | fstart_tcg::Remedy::None => {}
        }
        return;
    }
    fstart_tcg::measure_crtm_version(fstart_stage::tcg::CRTM_VERSION);
    let image = super::bios_directories(flash, generation)
        .and_then(|dirs| dirs.find(bios::RESET_IMAGE, None, None))
        .and_then(|(dir, entry)| dir.data(entry).ok());
    match image {
        Some(image) => fstart_tcg::measure_blob(
            0,
            EV_S_CRTM_CONTENTS,
            "fstart reset image",
            image.as_ptr() as u64,
            image,
        ),
        None => {
            fstart_log::error!("tpm: no reset image entry to measure");
            fstart_tcg::measurement_error();
        }
    }
}

/// The SMM handler, before it is installed.
pub(super) fn measure_smm(image: &'static [u8]) {
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
#[cfg(feature = "acpi")]
pub(super) fn acpi() -> Option<fstart_acpi::devices::tpm::TpmTisAcpi> {
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
