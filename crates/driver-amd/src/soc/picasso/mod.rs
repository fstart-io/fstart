// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h Raven/Picasso/Pollock register geometry, not Intel-compatible IP.
use super::Soc;
use crate::fch::gpio::{Bank, FAMILY17H_EVENTS};

#[cfg(test)]
mod tests;

/// The Raven/Picasso SoC, one die: [`crate::ccx::picasso`],
/// [`crate::df::picasso`], [`crate::nbio::picasso`], [`crate::dxio::picasso`],
/// [`crate::smu::v10`], the Sandstone FCH [`crate::fch::sandstone`] and the
/// ACP 3.x [`crate::acp`].
pub enum Picasso {}
impl Soc for Picasso {
    const MAX_DIES: usize = 1;
    const SMU: crate::smu::Mailbox = crate::smu::v10::MAILBOX;
    const DXIO_SERVICE: u32 = crate::smu::v10::DXIO_SERVICE;
    /// GPIO bank 0: pins 0-144, no pad 63.
    const GPIO_BANK: &'static Bank = &Bank {
        pins: 145,
        reserved: &[63],
        gpio_function: crate::fch::sandstone::pinmux::gpio_function,
        events: FAMILY17H_EVENTS,
    };
    /// I2C0-I2C4. I2C2 and I2C3 are masters; I2C4 is peripheral only.
    const I2C: &'static [usize] = &[
        0xfedc_2000,
        0xfedc_3000,
        0xfedc_4000,
        0xfedc_5000,
        0xfedc_6000,
    ];
    fn ioms_fabric_id(_: u8) -> u8 {
        9
    }
}
/// Microcode for PCO B0/B1 and RV2 A0, the three vendor .bin inputs.
impl crate::ccx::microcode::Family for Picasso {
    const SIGNATURES: &'static [u32] = &[0x00810f10, 0x00810f80, 0x00820f00];
    const MAX_PATCHES: usize = 3;
}
pub type Die<P> = crate::df::Die<Picasso, P>;
pub const ECAM: u64 = 0xf800_0000;
pub const ECAM_BUSES: u8 = 64;
pub const UART_BASES: [usize; 4] = [0xfedc9000, 0xfedca000, 0xfedce000, 0xfedcf000];
pub const UART_AOAC: [u8; 4] = [11, 12, 16, 26];
pub const EARLY_AOAC: &[u8] = &[17, 7, 8, 9, 27];
