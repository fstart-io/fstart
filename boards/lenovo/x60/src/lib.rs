//! Lenovo ThinkPad X60/X60s board support package.
//!
//! fstart runs in long mode, so only Core 2 Duo (Merom) fitted X60s boot;
//! the 32-bit-only Core Duo (Yonah) models are not supported.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod config;
pub mod mainboard;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;
// Host table/board-policy tests do not link firmware entry or its allocator.
#[cfg(all(
    not(test),
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    )
))]
mod stage;

/// Lenovo ThinkPad X60 board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::X60_SMBIOS_IDENTITY;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub use mainboard::X60Mainboard;
#[cfg(fstart_stage_env = "ram")]
pub use mainboard::x60_mainboard_dsdt_aml;
