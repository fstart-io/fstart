// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz, Vilboz14 and Vilboz360 share Dalboz wiring and a single native flow.
//! SKU differences come from EC CBI FW_CONFIG, not independent Rust board copies.
#![no_std]
pub mod dalboz;
pub mod policy;
pub mod vilboz;
use fstart_platform_amd::picasso::{BoardFacts, Config};
static CONFIG: Config = Config::new().uart(0).build();
pub struct Board;
impl BoardFacts for Board {
    const CONFIG: &'static Config = &CONFIG;
}

pub const FLASH_SIZE: u32 = 0x1000000;
pub const EFS_OFFSET: u32 = 0xe20000;
/// RAM-ID GPIOs must be sampled before GPIO86 is driven high.
pub const RAM_ID_PINS: [u8; 4] = [132, 90, 86, 69];
/// APCB triples are NOT the same encoding as the x86 IOMUX pad selectors.
/// Dalboz only populates channel 0 DIMM 0.
pub const APCB_RAM_ID: [[u8; 3]; 4] = [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]];
/// Set-0 DDR4 SPD index selected by the *four-bit* RAM strap. IDs 0/1 are empty.
pub const SPD_SET0_INDEX: [Option<u8>; 16] = [
    None,
    None,
    Some(1),
    Some(1),
    Some(1),
    Some(7),
    Some(7),
    Some(2),
    Some(9),
    Some(9),
    Some(1),
    Some(1),
    Some(9),
    None,
    None,
    None,
];
