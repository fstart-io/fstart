//! The SP3 settings store ([`crate::sp3::layout::NVRAM`]) and the options'
//! form handed to the UEFI payload ([`crate::options`] reads the store at
//! boot).

#[cfg(feature = "payload-uefi")]
use super::Ctx;

/// The settings store as the payload's variable storage, and the options'
/// form for its menu. The store is writable only if the SPI100 command
/// engine reads the FFS as the ROM window shows it.
#[cfg(feature = "payload-uefi")]
pub(crate) fn storage(
    ctx: &Ctx<'_>,
) -> Option<
    fstart_payload::crabefi::settings::FlashStorage<
        fstart_driver_amd::fch::spi::flash::Flash<crate::sp3::silicon::hw::Tsc>,
    >,
> {
    use fstart_driver_amd::fch::spi::flash::{ERASE_SIZE, Extent, Flash};
    use fstart_payload::crabefi::settings::FlashStorage;

    let nvram = crate::sp3::layout::NVRAM;
    let extent = Extent {
        offset: nvram.offset,
        size: nvram.size,
    };
    let probe = super::flash_slice(ctx.ffs.base, 64);
    // SAFETY: the FCH decodes SPI100 at its fixed base (FCH reset tasks);
    // the payload launcher is the only user of the command engine, and the
    // stage runs from DRAM.
    let Ok(flash) = (unsafe { Flash::new(ctx.soc.timer, extent, ctx.ffs_offset, probe) }) else {
        fstart_log::error!("options: SPI100 command engine unusable, settings are volatile");
        return None;
    };
    let writable = flash.writable();
    Some(FlashStorage::new(flash, nvram.size, ERASE_SIZE, writable))
}

/// The options' CFR form for the payload's menu.
#[cfg(feature = "payload-uefi")]
pub(crate) fn settings(
    options: &fstart_options::Report,
) -> fstart_payload::crabefi::settings::FirmwareSettings {
    fstart_payload::crabefi::settings::FirmwareSettings {
        cfr: fstart_options::cfr::encode(options),
        cmos: None,
    }
}
