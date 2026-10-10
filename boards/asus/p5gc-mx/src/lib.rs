//! ASUS P5GC-MX i945GC/ICH7 and desktop model-6FX support.
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
pub use mainboard::P5GC_MX_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::P5gcMxMainboard;
