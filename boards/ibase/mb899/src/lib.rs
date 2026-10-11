//! Experimental IBASE MB899: i945GM/ICH7-M and W83627EHG policy.
//! Protected-mode stages support Socket M Core Duo and Core 2 configurations.
#![no_std]
#[cfg(test)]
extern crate std;
pub mod config;
pub mod mainboard;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;
#[cfg(all(
    not(test),
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    )
))]
mod stage;
pub struct Board;
pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::MB899_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::Mb899Mainboard;
