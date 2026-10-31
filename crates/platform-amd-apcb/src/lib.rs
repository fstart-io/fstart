// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-side AMD platform APCB support. Silicon ABI knowledge lives here;
//! mainboard values and firmware provenance are supplied as data by callers.
pub type Result<T> = std::result::Result<T, String>;
#[path = "amd/apcb_layout.rs"]
mod apcb_layout;
#[path = "amd/apcb_source.rs"]
pub mod apcb_source;
#[path = "amd/apcb_toml.rs"]
pub mod apcb_toml;
#[path = "amd/board_data.rs"]
mod board_data;
pub use board_data::BoardData;
mod spd;
pub use spd::parse_spd_hex;
#[cfg(test)]
#[path = "amd/test_data.rs"]
mod test_data;
