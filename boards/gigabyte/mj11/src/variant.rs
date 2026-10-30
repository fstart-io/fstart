// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data the EC1 (F09) and EC0 (F02) vendor firmware differ in.

/// Written as `value & mask` while `read & mask` differs.
pub struct VrReg {
    pub reg: u8,
    pub value: u8,
    pub mask: u8,
}

/// The register image of a VR controller behind the PCA954x at 0x71.
pub struct VrImage {
    /// 7-bit
    pub address: u8,
    pub mux_channel: u8,
    /// Written 0 to unlock, 7 to lock.
    pub lock_reg: u8,
    pub regs: &'static [VrReg],
}

/// `reg = (reg & and_mask) | or_mask` on an I2C device.
pub struct I2cRmw {
    pub address: u8,
    pub reg: u8,
    pub and_mask: u8,
    pub or_mask: u8,
}

#[cfg(not(feature = "ec0"))]
#[path = "tables/ec1.rs"]
mod tables;
#[cfg(feature = "ec0")]
#[path = "tables/ec0.rs"]
mod tables;

pub use tables::{POST_TRAINING_SMN, REDRIVERS, VR_IMAGES};

/// The board variant, as the vendor names it.
#[cfg(not(feature = "ec0"))]
pub const NAME: &str = "MJ11-EC1";
#[cfg(feature = "ec0")]
pub const NAME: &str = "MJ11-EC0";
