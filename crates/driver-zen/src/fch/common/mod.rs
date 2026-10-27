//! Tasks both FCHs run, with their generation deltas (FCH_SP3 §2–3, §5).
//!
//! Order on the primary FCH (FCH_SP3 §2.1; the same on all generations):
//!
//! 1. PEI reset: [`reset::prelim`], [`reset::ab_bar`], [`crate::fch::spi::reset`],
//!    [`reset::hw_acpi`], SATA reset (`taishan::sata` / `huashan::sata`),
//!    [`crate::fch::lpc::reset`], [`reset::espi`], USB reset (`taishan::usb` /
//!    `huashan::usb`);
//! 2. DXE env: [`env::hw_acpi_pre`], [`env::hw_acpi`], [`crate::fch::lpc::env`],
//!    [`env::sd`], SATA env, (ZP) xGbE, [`crate::fch::alink::clock_gating`];
//! 3. late (ReadyToBoot): SATA late, [`late::hw_acpi`], (Milan) the D2 SW SMI;
//! 4. SMM: [`sx`].

pub mod env;
pub mod late;
pub mod reset;
pub mod sx;

use super::Op;
use crate::soc::Generation;

/// A generation whose support is not built in: fail closed.
#[cold]
pub(crate) fn not_built(generation: Generation) -> ! {
    panic!("FCH: {} support not built", generation.name())
}

pub(crate) fn acpi_mmio_table(generation: Generation) -> &'static [Op] {
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => super::tables::ACPI_MMIO_NAPLES,
        #[cfg(feature = "rome")]
        Generation::Rome => super::tables::ACPI_MMIO_ROME,
        #[cfg(feature = "milan")]
        Generation::Milan => super::tables::ACPI_MMIO_MILAN,
        #[allow(unreachable_patterns)]
        g => not_built(g),
    }
}

pub(crate) fn sci_map(generation: Generation) -> &'static [Op] {
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => super::tables::SCI_MAP_NAPLES,
        #[cfg(feature = "rome")]
        Generation::Rome => super::tables::SCI_MAP_ROME,
        #[cfg(feature = "milan")]
        Generation::Milan => super::tables::SCI_MAP_MILAN,
        #[allow(unreachable_patterns)]
        g => not_built(g),
    }
}
