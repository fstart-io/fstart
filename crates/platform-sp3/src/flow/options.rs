//! The settings store ([`crate::layout::NVRAM`]): option overrides read at
//! boot, and the store and the options' form handed to the UEFI payload.
//!
//! The store is CrabEFI's EFI variable store, so the overrides are only
//! read in a build with the UEFI payload, which also brings the menu that
//! writes them. Other payloads boot the board's configuration.

use fstart_options::{Knob, Report, Source};

#[cfg(feature = "payload-uefi")]
use super::Ctx;

/// The overrides stored in `nvram`, the ROM view of the settings store.
pub(crate) fn source(nvram: Option<&'static [u8]>) -> impl Source {
    #[cfg(feature = "payload-uefi")]
    {
        use fstart_payload::crabefi::settings::VarstoreOptions;
        nvram.map(VarstoreOptions::new)
    }
    #[cfg(not(feature = "payload-uefi"))]
    {
        let _ = nvram;
        fstart_options::NoSource
    }
}

/// [`fstart_options::resolve_checked`] from the settings store, logged.
pub(crate) fn resolve<C: Clone>(
    nvram: Option<&'static [u8]>,
    knobs: &'static [Knob<C>],
    config: &mut C,
    report: &mut Report,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
) {
    fstart_log::option::resolve(knobs, &source(nvram), config, report, check);
}

/// The settings store as the payload's variable storage, and the options'
/// form for its menu. The store is writable only if the SPI100 command
/// engine reads the FFS as the ROM window shows it.
#[cfg(feature = "payload-uefi")]
pub(crate) fn storage(
    ctx: &Ctx,
) -> Option<
    fstart_payload::crabefi::settings::FlashStorage<
        fstart_driver_amd::fch::spi::flash::Flash<crate::hw::Tsc>,
    >,
> {
    use fstart_driver_amd::fch::spi::flash::{ERASE_SIZE, Extent, Flash};
    use fstart_payload::crabefi::settings::FlashStorage;

    let nvram = crate::layout::NVRAM;
    let extent = Extent {
        offset: nvram.offset,
        size: nvram.size,
    };
    let probe = super::flash_slice(ctx.ffs.base, 64);
    // SAFETY: the FCH decodes SPI100 at its fixed base (FCH reset tasks);
    // the payload launcher is the only user of the command engine, and the
    // stage runs from DRAM.
    let Ok(flash) = (unsafe { Flash::new(ctx.timer, extent, ctx.ffs_offset, probe) }) else {
        fstart_log::error!("options: SPI100 command engine unusable, settings are volatile");
        return None;
    };
    let writable = flash.writable();
    Some(FlashStorage::new(flash, nvram.size, ERASE_SIZE, writable))
}

/// The options' CFR form for the payload's menu.
#[cfg(feature = "payload-uefi")]
pub(crate) fn settings(ctx: &Ctx) -> fstart_payload::crabefi::settings::FirmwareSettings {
    fstart_payload::crabefi::settings::FirmwareSettings {
        cfr: fstart_options::cfr::encode(&ctx.options),
        cmos: None,
    }
}
