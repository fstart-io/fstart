// SPDX-License-Identifier: MIT OR Apache-2.0
//! The options the user may override (`fstart_options`), for every SoC:
//! the overrides in the SoC's settings store, applied over the board's
//! values ([`resolve`]). The store is CrabEFI's EFI variable store, which
//! its menu writes. Every payload's boot reads it; only the UEFI payload
//! can change it. A SoC without a store gets the board's values.

use fstart_options::varstore::VarstoreOptions;
use fstart_options::{Knob, Report, Source};

/// The overrides stored in `nvram`, the ROM view of the settings store.
pub fn source(nvram: Option<&'static [u8]>) -> impl Source {
    nvram.map(VarstoreOptions::new)
}

/// [`fstart_options::resolve_checked`] from the settings store, logged.
pub fn resolve<C: Clone>(
    nvram: Option<&'static [u8]>,
    knobs: &'static [Knob<C>],
    config: &mut C,
    report: &mut Report,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
) {
    fstart_log::option::resolve_into(knobs, &source(nvram), config, report, check);
}

#[cfg(all(feature = "payload-uefi", not(feature = "ab-rw")))]
pub(crate) type FlashStorage = fstart_payload::crabefi::settings::FlashStorage<
    fstart_driver_amd::fch::spi::flash::Flash<fstart_driver_amd::ccx::tsc::Tsc>,
>;

/// Bind a reserved flash extent to CrabEFI. The SPI command engine must
/// read the same FFS bytes as the stage's ROM window before allowing writes.
///
/// # Safety
/// SPI100 must be decoded at its fixed base. The payload owns it exclusively
/// and the caller runs from RAM. The extent is reserved for EFI variables.
#[cfg(all(feature = "payload-uefi", not(feature = "ab-rw")))]
pub(crate) unsafe fn storage(
    clock: fstart_driver_amd::ccx::tsc::Tsc,
    extent: fstart_driver_amd::fch::spi::flash::Extent,
    probe_offset: u32,
    probe: &[u8],
) -> Option<FlashStorage> {
    use fstart_driver_amd::fch::spi::flash::{ERASE_SIZE, Flash};

    // SAFETY: the caller owns the controller and its extent.
    let Ok(flash) = (unsafe { Flash::new(clock, extent, probe_offset, probe) }) else {
        fstart_log::error!("options: SPI100 command engine unusable, settings are volatile");
        return None;
    };
    let writable = flash.writable();
    Some(FlashStorage::new(flash, extent.size, ERASE_SIZE, writable))
}

/// The options' CFR form for the payload's menu.
#[cfg(feature = "payload-uefi")]
pub(crate) fn settings(options: &Report) -> fstart_payload::crabefi::settings::FirmwareSettings {
    fstart_payload::crabefi::settings::FirmwareSettings {
        cfr: fstart_options::cfr::encode(options),
        cmos: None,
    }
}
