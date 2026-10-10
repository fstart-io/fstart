//! Gigabyte GA-D510UD Pineview/NM10 support.
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
pub struct Board;
pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::GA_D510UD_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::GaD510udMainboard;
