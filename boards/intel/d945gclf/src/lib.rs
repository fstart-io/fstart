//! Intel D945GCLF board support package.

#![no_std]

pub mod config;
pub mod mainboard;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
mod stage;

/// Intel D945GCLF board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::D945GCLF_SMBIOS_DESC;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
pub use mainboard::D945GclfMainboard;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::d945gclf_mainboard_dsdt_aml;
