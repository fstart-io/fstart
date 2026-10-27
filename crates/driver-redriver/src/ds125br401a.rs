// SPDX-License-Identifier: MIT OR Apache-2.0
//! TI DS125BR401A SMBus channel configuration (SNLS466A, table 12).

use embedded_hal::i2c::I2c;
use tock_registers::register_bitfields;

register_bitfields![u8,
    SlaveControl [
        REGISTER_ENABLE OFFSET(3) NUMBITS(1) [],
        /// Datasheet requires bit 4 to be set.
        REQUIRED_ONE OFFSET(4) NUMBITS(1) []
    ]
];

const SLAVE_CONTROL: u8 = 0x06;
const CHANNEL_EQ: [u8; 8] = [0x0f, 0x16, 0x1d, 0x24, 0x2c, 0x33, 0x3a, 0x41];

/// Full channel register bytes chosen for the board's signal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub equalization: u8,
    pub output_voltage: u8,
    pub deemphasis: u8,
}

pub struct Ds125Br401a<I> {
    bus: I,
    address: u8,
}

impl<I: I2c> Ds125Br401a<I> {
    pub fn new(bus: I, address: u8) -> Self {
        Self { bus, address }
    }

    /// Enable register control without CRC, then program EQ, VOD and DEM
    /// for all eight channels. Preserve this ordering across channels.
    pub fn configure(&mut self, settings: Settings) -> Result<(), I::Error> {
        let control = SlaveControl::REGISTER_ENABLE::SET + SlaveControl::REQUIRED_ONE::SET;
        self.bus
            .write(self.address, &[SLAVE_CONTROL, control.value])?;
        [
            (0, settings.equalization),
            (1, settings.output_voltage),
            (2, settings.deemphasis),
        ]
        .into_iter()
        .flat_map(|(offset, value)| {
            CHANNEL_EQ
                .into_iter()
                .map(move |base| (base + offset, value))
        })
        .try_for_each(|(register, value)| self.bus.write(self.address, &[register, value]))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};
    use std::{vec, vec::Vec};

    #[test]
    fn pcie_tuning_follows_the_register_sequence() {
        let expected: Vec<_> = core::iter::once(Transaction::write(0x58, vec![6, 0x18]))
            .chain(
                [
                    ([0x0f, 0x16, 0x1d, 0x24, 0x2c, 0x33, 0x3a, 0x41], 0),
                    ([0x10, 0x17, 0x1e, 0x25, 0x2d, 0x34, 0x3b, 0x42], 0),
                    ([0x11, 0x18, 0x1f, 0x26, 0x2e, 0x35, 0x3c, 0x43], 0x11),
                ]
                .into_iter()
                .flat_map(|(registers, value)| {
                    registers
                        .into_iter()
                        .map(move |register| Transaction::write(0x58, vec![register, value]))
                }),
            )
            .collect();
        let mut bus = Mock::new(&expected);
        Ds125Br401a::new(&mut bus, 0x58)
            .configure(Settings {
                equalization: 0,
                output_voltage: 0,
                deemphasis: 0x11,
            })
            .unwrap();
        bus.done();
    }
}
