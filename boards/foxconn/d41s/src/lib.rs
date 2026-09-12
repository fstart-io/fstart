//! Foxconn D41S board support package.

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

/// Foxconn D41S board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::D41S_SMBIOS_DESC;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
pub use mainboard::D41SMainboard;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::d41s_mainboard_dsdt_aml;
