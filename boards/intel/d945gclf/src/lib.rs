//! Intel D945GCLF board support package.

#![no_std]

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
#[cfg(feature = "stage")]
mod stage;

/// Intel D945GCLF board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(feature = "acpi")]
pub use mainboard::d945gclf_mainboard_dsdt_aml;
#[cfg(feature = "stage")]
pub use mainboard::D945GclfMainboard;
#[cfg(feature = "smbios")]
pub use mainboard::D945GCLF_SMBIOS_DESC;
