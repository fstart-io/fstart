//! Debug consoles, compiled in with `fbuild --debug`: release images carry
//! none. In every stage the platform adds the selected ones next to the
//! board's console (fstart-log's debug consoles), so a board needs no code
//! for them, even one with no console of its own (`NoConsole`).
//!
//! * `--debug ehci`: the EHCI debug port console (`fstart-driver-usbdebug`)
//!   on the port the chipset config reserves
//!   ([`crate::IntelPlatformConfig::ehci_debug`]).
//! * `--debug flash-console`: the SPI flash log
//!   (`fstart-driver-flash-console`, coreboot `CONSOLE_SPI_FLASH`) through
//!   the ICH SPI controller. The image builder reserves
//!   [`FLASH_CONSOLE_SIZE`] bytes after the memory cache at the beginning of
//!   the BIOS region.
//!
//! [`FLASH_CONSOLE_SIZE`]: crate::facts::FLASH_CONSOLE_SIZE

/// Add the debug consoles the build selects. ECAM and RCBA must be mapped;
/// the board's console is already installed. `flash_log` says nothing else
/// reads the flash while the log programs it (see the flash module).
#[cfg(feature = "stage")]
pub(crate) fn install<B: crate::IntelBoardFacts>(flash_log: bool) {
    #[cfg(feature = "ehci-console")]
    ehci::install::<B>();
    #[cfg(all(
        feature = "flash-console",
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    if flash_log {
        flash::install::<B>();
    }
    let _ = flash_log;
}

#[cfg(all(feature = "stage", feature = "ehci-console"))]
mod ehci {
    use crate::IntelPlatformConfig;
    use fstart_core::services::ConsoleDevice;
    use fstart_driver_usbdebug::EhciDebug;
    use fstart_log::StaticConsole;

    static EHCI: StaticConsole<EhciDebug> = StaticConsole::new();

    pub(super) fn install<B: crate::IntelBoardFacts>() {
        let Some(config) = B::CONFIG.ehci_debug() else {
            return;
        };
        let Ok(mut console) = EhciDebug::new(config) else {
            return;
        };
        // An absent gadget leaves it offline; it never stops the boot.
        if console.init().is_ok() {
            EHCI.install_debug(console);
        }
    }
}

#[cfg(all(
    feature = "stage",
    feature = "flash-console",
    any(target_arch = "x86", target_arch = "x86_64")
))]
mod flash {
    use fstart_core::FlashLayout;
    use fstart_driver_flash_console::{FlashConsole, XipImage};
    use fstart_driver_intel::southbridge::spi::{CacheSpi, FlashExtent};
    use fstart_log::StaticConsole;

    static LOG: StaticConsole<FlashConsole<CacheSpi>> = StaticConsole::new();

    pub(super) fn install<B: crate::IntelBoardFacts>() {
        let facts = B::FACTS;
        let bios_offset = match facts.flash {
            FlashLayout::IntelIfd(layout) => layout.bios_region().map_or(0, |bios| bios.offset),
            FlashLayout::X86Legacy(_) => 0,
        };
        let size = crate::facts::FLASH_CONSOLE_SIZE;
        let extent = FlashExtent {
            offset: bios_offset + facts.mutable_prefix(true) - size,
            size,
            chip_size: facts.flash_size,
        };
        // CacheSpi reaches the LPC bridge through the stage's ECAM accessor,
        // whose typed overlay panics while it is unbound.
        if fstart_pci::ecam::base() == 0 {
            return;
        }
        // SAFETY: the image builder reserves the extent as an erased `console`
        // region nothing else writes; an XIP bootblock passes its image, which
        // the console caches before every program cycle, and postcar runs on
        // DRAM page tables (install's `flash_log`).
        let Ok(spi) = (unsafe { CacheSpi::new(extent) }) else {
            return;
        };
        if !spi.writable() {
            return;
        }
        let console = FlashConsole::open(spi, size, running_xip_image(facts.flash_size));
        if console.online() {
            LOG.install_debug(console);
        }
    }

    /// The running image when it executes in place from the flash mapping
    /// below 4 GiB: the bootblock. Later stages run from RAM.
    fn running_xip_image(flash_size: u32) -> Option<XipImage> {
        #[cfg(feature = "bundle-bootblock")]
        {
            let [image, _] = fstart_stage::boot::running_stage_windows().ok()?;
            let flash_base = (1u64 << 32) - u64::from(flash_size);
            let end = image.start.checked_add(image.size)?;
            (image.start >= flash_base && end <= 1 << 32).then_some(XipImage {
                base: image.start as usize,
                size: image.size as usize,
            })
        }
        #[cfg(not(feature = "bundle-bootblock"))]
        {
            let _ = flash_size;
            None
        }
    }
}
