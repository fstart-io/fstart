//! Tasks both FCHs run, with their generation deltas (FCH_SP3 §2–3, §5).
//!
//! Order on the primary FCH (FCH_SP3 §2.1; the same on all generations):
//!
//! 1. PEI reset: [`reset::prelim`], [`reset::ab_bar`], [`crate::fch::spi::reset`],
//!    [`reset::hw_acpi`], SATA reset (`taishan::die_sata` / `huashan::sata`),
//!    [`crate::fch::lpc::reset`], [`reset::espi`], USB reset (`taishan::die_usb` /
//!    `huashan::usb`);
//! 2. DXE env: [`env::hw_acpi_pre`], [`env::hw_acpi`], [`crate::fch::lpc::env`],
//!    [`env::sd`], SATA env, (ZP) xGbE, [`crate::fch::alink::clock_gating`];
//! 3. late (ReadyToBoot): SATA late, [`late::hw_acpi`], (Milan) the D2 SW SMI;
//! 4. SMM: [`sx`].

pub mod env;
pub mod late;
pub mod reset;
pub mod sx;

use crate::cpu::Silicon;
use crate::window::Op;

/// A generation whose support is not built in: fail closed.
#[cold]
pub(crate) fn not_built(generation: Silicon) -> ! {
    panic!("FCH: {} support not built", generation.name())
}

pub(crate) fn acpi_mmio_table(generation: Silicon) -> &'static [Op] {
    match generation {
        Silicon::Zp => super::tables::ACPI_MMIO_NAPLES,
        Silicon::Ssp => super::tables::ACPI_MMIO_ROME,
        Silicon::Gn => super::tables::ACPI_MMIO_MILAN,
        #[allow(unreachable_patterns)]
        g => not_built(g),
    }
}

pub(crate) fn sci_map(generation: Silicon) -> &'static [Op] {
    match generation {
        Silicon::Zp => super::tables::SCI_MAP_NAPLES,
        Silicon::Ssp => super::tables::SCI_MAP_ROME,
        Silicon::Gn => super::tables::SCI_MAP_MILAN,
        #[allow(unreachable_patterns)]
        g => not_built(g),
    }
}
