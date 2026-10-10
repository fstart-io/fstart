//! Winbond W83627THF/THG logical devices.
//!
//! ID 0x828x includes the revision nibble. Keyboard and mouse share LDN 5;
//! their IRQs must occupy separate slots. Hardware monitoring uses an indexed
//! register pair at HWMBASE+5/+6, not the two-base ITE EC layout.

use crate::{IoResource, IrqResource, SuperIo, SuperIoChip};
use tock_registers::interfaces::Writeable;
use tock_registers::register_bitfields;

register_bitfields![u8,
    KBC_CONFIG [
        CLOCK OFFSET(6) NUMBITS(2) [Mhz6 = 0, Mhz8 = 1, Mhz12 = 2, Mhz16 = 3],
        PORT92 OFFSET(2) NUMBITS(1) [],
        A20_SPEEDUP OFFSET(1) NUMBITS(1) [],
        RESET_SPEEDUP OFFSET(0) NUMBITS(1) [],
    ],
    SERIAL_CONFIG [
        IR_LOCATION OFFSET(6) NUMBITS(1) [UartPins = 0, IrPins = 1],
        IR_MODE OFFSET(3) NUMBITS(3) [Disabled = 0],
        HALF_DUPLEX OFFSET(2) NUMBITS(1) [],
        TX_INVERT OFFSET(1) NUMBITS(1) [],
        RX_INVERT OFFSET(0) NUMBITS(1) [],
    ],
];

pub struct W83627thgChip;

impl SuperIoChip for W83627thgChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0x8280;
    const CHIP_ID_MASK: u16 = 0xfff0;
    const COM1_LDN: Option<u8> = Some(2);
    const COM2_LDN: Option<u8> = Some(3);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(5);
    const MOUSE_IRQ_SECONDARY: bool = true;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(1);

    fn chip_init(base: u16) {
        use fstart_core::pio::IndexedPioRegister;
        let ldn = IndexedPioRegister::<()>::new(base, base + 1, 7);
        ldn.set(3);
        IndexedPioRegister::<SERIAL_CONFIG::Register>::new(base, base + 1, 0xf1).write(
            SERIAL_CONFIG::IR_LOCATION::UartPins
                + SERIAL_CONFIG::IR_MODE::Disabled
                + SERIAL_CONFIG::HALF_DUPLEX::CLEAR
                + SERIAL_CONFIG::TX_INVERT::CLEAR
                + SERIAL_CONFIG::RX_INVERT::CLEAR,
        );
        ldn.set(5);
        IndexedPioRegister::<KBC_CONFIG::Register>::new(base, base + 1, 0xf0).write(
            KBC_CONFIG::CLOCK::Mhz12
                + KBC_CONFIG::PORT92::CLEAR
                + KBC_CONFIG::A20_SPEEDUP::CLEAR
                + KBC_CONFIG::RESET_SPEEDUP::CLEAR,
        );
    }
}

pub type W83627thg = SuperIo<W83627thgChip>;

impl W83627thg {
    /// Allocate the HWM register block and leave its interrupt disconnected.
    pub fn enable_hwmon(&mut self, base: u16) {
        self.enter_config();
        let mut device = self.logical_device(0x0b);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(0));
        device.set_enabled(true);
        self.exit_config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_share_id_and_mouse_does_not_replace_keyboard_irq() {
        for id in 0x8280..=0x828f {
            assert_eq!(id & W83627thgChip::CHIP_ID_MASK, W83627thgChip::CHIP_ID);
        }
        assert_eq!(W83627thgChip::KBC_LDN, W83627thgChip::MOUSE_LDN);
        const { assert!(W83627thgChip::MOUSE_IRQ_SECONDARY) };
    }
}
