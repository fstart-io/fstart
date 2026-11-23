//! The SP3 settings store ([`crate::sp3::layout::NVRAM`]) and the options'
//! form handed to the UEFI payload ([`crate::options`] reads the store at
//! boot).

#[cfg(all(feature = "payload-uefi", not(feature = "ab-rw")))]
use super::Ctx;

/// The settings store as the payload's variable storage, and the options'
/// form for its menu. The store is writable only if the SPI100 command
/// engine reads the FFS as the ROM window shows it.
#[cfg(all(feature = "payload-uefi", not(feature = "ab-rw")))]
pub(crate) fn storage(ctx: &Ctx<'_>) -> Option<crate::options::FlashStorage> {
    use fstart_driver_amd::fch::spi::flash::Extent;

    let nvram = crate::sp3::layout::NVRAM;
    let extent = Extent {
        offset: nvram.offset,
        size: nvram.size,
    };
    let probe = super::flash_slice(ctx.ffs.base, 64);
    // SAFETY: the FCH decodes SPI100 at its fixed base (FCH reset tasks);
    // the payload launcher is the only user of the command engine, and the
    // stage runs from DRAM.
    unsafe { crate::options::storage(ctx.soc.timer, extent, ctx.ffs_offset, probe) }
}
