//! Lenovo ThinkPad X61 board support package.

#![no_std]

pub mod config;
pub mod devices;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub use config::*;
pub use devices::*;
pub use mainboard::{
    x61_mainboard_config, LenovoX61Mainboard, LenovoX61MainboardConfig, LenovoX61Southbridge,
    X61_SMBIOS_DESC,
};
