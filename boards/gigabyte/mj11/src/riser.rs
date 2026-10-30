// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCIE_1 riser (vendor AmdCpmOemInitPeim, Setup "PCIE_1" Auto, the
//! default): its ID from the EEPROM behind the riser mux on I2C3, else
//! from four strap GPIOs, and the slot bifurcation it asks for (sp4r2
//! BOARD_MJ11 §3.2).
use embedded_hal::i2c::I2c;

/// The PCA9545 in front of the riser EEPROM and its channel.
const MUX: u8 = 0x74;
const MUX_CHANNEL: u8 = 1 << 2;
const EEPROM: u8 = 0x57;
const EEPROM_ID: u8 = 0x11;
/// The ID straps, most significant first.
pub const STRAPS: [u8; 4] = [152, 151, 4, 3];

/// The riser ID from its EEPROM (the vendor swaps IDs 1 and 2 and maps 4
/// to 7), `None` if the riser doesn't answer.
pub fn eeprom_id<I: I2c>(i2c: &mut I) -> Option<u8> {
    i2c.write(MUX, &[0, MUX_CHANNEL]).ok()?;
    let mut byte = [0];
    // The vendor reads register 1 first and drops it.
    i2c.write_read(EEPROM, &[1], &mut byte).ok()?;
    i2c.write_read(EEPROM, &[EEPROM_ID], &mut byte).ok()?;
    Some(match byte[0] {
        1 => 2,
        2 => 1,
        4 => 7,
        id => id,
    })
}

/// The riser ID from the strap levels in [`STRAPS`] order; 0 is none.
pub fn strap_id(levels: [bool; 4]) -> Option<u8> {
    let id = levels
        .iter()
        .fold(0u8, |id, &level| id << 1 | u8::from(level));
    (id != 0).then_some(id)
}

/// How a riser splits the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bifurcation {
    X4x4x4x4,
    /// Only the upper two x4 links.
    X4x4,
    X8x4x4,
    X8x8,
}

/// The vendor's riser ID to mode table; `None`: keep the board's default.
pub const fn bifurcation(id: u8) -> Option<Bifurcation> {
    match id {
        0 | 4 | 5 | 8 | 0xc | 0xd => Some(Bifurcation::X4x4x4x4),
        1 | 9 | 0x10 => Some(Bifurcation::X4x4),
        2 | 6 | 0xa | 0xe | 0x11 => Some(Bifurcation::X8x4x4),
        3 | 0xb => Some(Bifurcation::X8x8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_modes() {
        assert_eq!(strap_id([true, false, false, true]), Some(9));
        assert_eq!(strap_id([false; 4]), None);
        assert_eq!(bifurcation(9), Some(Bifurcation::X4x4));
        assert_eq!(bifurcation(0xf), None);
        assert_eq!(crate::dxio::with_riser(Bifurcation::X8x8).len(), 8);
    }
}
