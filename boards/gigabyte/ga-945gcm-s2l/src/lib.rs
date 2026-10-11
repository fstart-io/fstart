//! Shared Gigabyte GA-945GCM-S2L/S2C wiring and board sequencing.
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
pub use mainboard::GA945GCM_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::Ga945GcmMainboard;
