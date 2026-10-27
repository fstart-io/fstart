//! Board identity and PCIe straps from RXM100CB's I2C devices.
//!
//! AmdBoardIdPei reads the M24LC128 on controller 5, address 0x50; the
//! board ID is byte 0x10. AmdCpmOemInitPeim reads the PCA9535A at 0x20
//! on controller 0. Detection does not write EEPROM data or GPIO outputs.

use core::fmt::Debug;
use eeprom24x::{Eeprom24x, SlaveAddr};
use embedded_hal::i2c::I2c;
use fstart_core::services::{Timer, TimerDelay};
use fstart_driver_amd::sp3::fch::smbus::{I2C_TIMING, i2c_base};
use fstart_driver_i2c::designware::DesignWare;
use fstart_platform_sp3::board::BoardError;
use pca9535::{Expander, Pca9535Immediate, Register};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub board_id: u8,
    pub straps: u16,
}

fn failed(bus: u8, device: &str, cause: impl Debug) -> BoardError {
    let detail = alloc::format!("{cause:?}");
    fstart_log::error!("ethanolx: I2C{} {}: {}", bus, device, detail.as_str());
    BoardError::Device
}

pub fn detect(timer: &dyn Timer) -> Result<Detected, BoardError> {
    let controller = |index| {
        // SAFETY: FCH APB windows are decoded since reset. The BSP owns
        // these controllers before DXIO; no AP or payload is using them.
        // Like the vendor I2C PEIM, preserve the existing power/pad routing.
        unsafe { DesignWare::new(i2c_base(index) as usize, &I2C_TIMING, TimerDelay(timer)) }
            .map_err(|error| failed(index, "controller", error))
    };
    let mut pcie = controller(0)?;
    let mut eeprom = controller(5)?;
    read(&mut pcie, &mut eeprom)
}

fn read<I: I2c>(pcie: &mut I, eeprom: &mut I) -> Result<Detected, BoardError> {
    let board_id = Eeprom24x::new_24x128(eeprom, SlaveAddr::Default)
        .read_byte(0x10)
        .map_err(|error| failed(5, "board-ID EEPROM 0x50", error))?;
    let mut straps = 0;
    Pca9535Immediate::new(pcie, 0x20)
        .read_halfword(Register::InputPort0, &mut straps)
        .map_err(|error| failed(0, "PCIe straps GPIO 0x20", error))?;
    Ok(Detected {
        board_id,
        // The crate returns the first bank as the high byte; vendor
        // strap nibbles number GPIO bank 0 as the low byte.
        straps: straps.swap_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::i2c::{ErrorKind, NoAcknowledgeSource};
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

    #[test]
    fn reads_identity_and_straps_without_changing_device_state() {
        let mut eeprom = Mock::new(&[Transaction::write_read(0x50, vec![0, 0x10], vec![0x30])]);
        let mut pcie = Mock::new(&[Transaction::write_read(0x20, vec![0], vec![0x8c, 0xaf])]);
        assert_eq!(
            read(&mut pcie, &mut eeprom),
            Ok(Detected {
                board_id: 0x30,
                straps: 0xaf8c
            })
        );
        eeprom.done();
        pcie.done();
    }

    #[test]
    fn unreadable_identity_is_not_a_default_board_configuration() {
        let mut eeprom = Mock::new(&[Transaction::write_read(0x50, vec![0, 0x10], vec![0])
            .with_error(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address))]);
        let mut pcie = Mock::new(&[]);
        assert_eq!(read(&mut pcie, &mut eeprom), Err(BoardError::Device));
        eeprom.done();
        pcie.done();
    }
}
