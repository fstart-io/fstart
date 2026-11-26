//! The SP3 settings store ([`crate::sp3::layout::NVRAM`]) and the options'
//! form handed to the UEFI payload ([`crate::options`] reads the store at
//! boot).

#[cfg(feature = "payload-uefi")]
use super::Ctx;

/// The settings store as the payload's variable storage, and the options'
/// form for its menu. The store is writable only if the SPI100 command
/// engine reads the FFS as the ROM window shows it.
#[cfg(feature = "payload-uefi")]
pub(crate) fn storage(ctx: &Ctx<'_>) -> Option<crate::options::FlashStorage> {
    use fstart_driver_amd::fch::spi::flash::Extent;

    let nvram = crate::sp3::layout::NVRAM;
    let extent = Extent {
        offset: nvram.offset,
        size: nvram.size,
    };
    let probe = crate::media::flash_slice(ctx.ffs.base, 64);
    // SAFETY: the FCH decodes SPI100 at its fixed base (FCH reset tasks);
    // the payload launcher is the only user of the command engine, and the
    // stage runs from DRAM.
    unsafe { crate::options::storage(&ctx.soc.fch, ctx.soc.timer, extent, ctx.ffs_offset, probe) }
}
