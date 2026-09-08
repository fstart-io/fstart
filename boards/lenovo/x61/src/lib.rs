//! Lenovo ThinkPad X61 board support package.

#![no_std]

#[cfg(test)]
extern crate std;

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

/// Lenovo ThinkPad X61 board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::X61_SMBIOS_DESC;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
pub use mainboard::X61Mainboard;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::x61_mainboard_dsdt_aml;
