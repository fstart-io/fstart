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
use fstart_driver_redriver::ds125br401a::{Ds125Br401a, Settings};
use fstart_platform_sp3::board::BoardError;
use pca9535::{Expander, GPIOBank, Pca9535Immediate, Register, StandardExpanderInterface};
use xca9548a::{SlaveAddr as MuxAddress, Xca9545a};

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

fn controller(index: u8, timer: &dyn Timer) -> Result<DesignWare<TimerDelay<'_>>, BoardError> {
    // SAFETY: FCH APB windows are decoded since reset. The BSP owns
    // these controllers before DXIO; no AP or payload is using them.
    // Like the vendor I2C PEIM, preserve the existing power/pad routing.
    unsafe { DesignWare::new(i2c_base(index) as usize, &I2C_TIMING, TimerDelay(timer)) }
        .map_err(|error| failed(index, "controller", error))
}

pub fn detect(timer: &dyn Timer) -> Result<Detected, BoardError> {
    let mut pcie = controller(0, timer)?;
    let mut eeprom = controller(5, timer)?;
    read(&mut pcie, &mut eeprom)
}

fn read<I: I2c>(pcie: &mut I, eeprom: &mut I) -> Result<Detected, BoardError> {
    let board_id = Eeprom24x::new_24x128(eeprom, SlaveAddr::Default)
        .read_byte(0x10)
        .map_err(|error| failed(5, "board-ID EEPROM 0x50", error))?;
    Ok(Detected {
        board_id,
        straps: read_straps(pcie)?,
    })
}

fn read_straps<I: I2c>(pcie: &mut I) -> Result<u16, BoardError> {
    let mut straps = 0;
    Pca9535Immediate::new(pcie, 0x20)
        .read_halfword(Register::InputPort0, &mut straps)
        .map_err(|error| failed(0, "PCIe straps GPIO 0x20", error))?;
    // The crate returns the first bank as the high byte; vendor strap
    // nibbles number GPIO bank 0 as the low byte.
    Ok(straps.swap_bytes())
}

/// The caller has checked the board revision. Follow the vendor order:
/// prepare routing GPIO, then sample the authoritative slot straps.
pub fn initialize(timer: &dyn Timer, detected: Detected) -> Result<Detected, BoardError> {
    let mut pcie = controller(0, timer)?;
    routing_gpio(&mut pcie)?;
    Ok(Detected {
        straps: read_straps(&mut pcie)?,
        ..detected
    })
}

/// Card-specific programming follows successful lane-policy validation.
pub fn configure_redrivers(timer: &dyn Timer, redrivers: [bool; 4]) -> Result<(), BoardError> {
    if !redrivers.into_iter().any(|enabled| enabled) {
        return Ok(());
    }
    let mut pcie = controller(0, timer)?;
    for index in redrivers
        .into_iter()
        .enumerate()
        .filter_map(|(i, enabled)| enabled.then_some(i))
    {
        tune_redrivers(&mut pcie, index as u8)?;
    }
    Ok(())
}

fn routing_gpio<I: I2c>(bus: &mut I) -> Result<(), BoardError> {
    let mut gpio = Pca9535Immediate::new(bus, 0x22);
    let error = |cause| failed(0, "PCIe routing GPIO 0x22", cause);
    let (mut config, mut output, mut config_after, mut output_after) = (0, 0, 0, 0);
    gpio.read_byte(Register::ConfigurationPort0, &mut config)
        .map_err(error)?;
    // RXM100CB: configure P0.1 as output, then drive it low. All other pins
    // retain their state; do not assign a guessed signal name to this pin.
    gpio.pin_into_output(GPIOBank::Bank0, 1).map_err(error)?;
    gpio.read_byte(Register::OutputPort0, &mut output)
        .map_err(error)?;
    gpio.pin_set_low(GPIOBank::Bank0, 1).map_err(error)?;
    gpio.read_byte(Register::ConfigurationPort0, &mut config_after)
        .map_err(error)?;
    gpio.read_byte(Register::OutputPort0, &mut output_after)
        .map_err(error)?;
    fstart_log::info!(
        "ethanolx: GPIO22 config {:#x}->{:#x}, output {:#x}->{:#x}",
        config,
        config_after,
        output,
        output_after
    );
    if config_after != config & !2 || output_after != output & !2 {
        fstart_log::error!("ethanolx: GPIO22 output readback mismatch");
        return Err(BoardError::Device);
    }
    Ok(())
}

fn tune_redrivers<I: I2c>(bus: &mut I, group: u8) -> Result<(), BoardError> {
    let address = MuxAddress::Alternative(false, group & 2 != 0, group & 1 != 0);
    let mut mux = Xca9545a::new(bus, address);
    for chip in 0x58..=0x5b {
        fstart_log::info!(
            "ethanolx: PCIe switch {:#x}, redriver {:#x}",
            0x70 + group,
            chip
        );
        mux.select_channels(1)
            .map_err(|error| failed(0, "PCIe switch", error))?;
        // RXM100CB four-chip PCIe table at 0xffd72994, not the three-chip
        // SATA/xGbE table. These are the vendor's full register bytes.
        let programming = Ds125Br401a::new(&mut mux, chip)
            .configure(Settings {
                equalization: 0,
                output_voltage: 0,
                deemphasis: 0x11,
            })
            .map_err(|error| failed(0, "PCIe redriver", error));
        // Always attempt deselection, including after a programming failure.
        let deselection = mux
            .select_channels(0)
            .map_err(|error| failed(0, "PCIe switch deselect", error));
        programming.and(deselection)?;
    }
    Ok(())
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
    fn routing_output_preserves_other_pins_and_checks_readback() {
        let mut bus = Mock::new(&[
            Transaction::write_read(0x22, vec![6], vec![0xff]),
            Transaction::write_read(0x22, vec![6], vec![0xff]),
            Transaction::write(0x22, vec![6, 0xfd]),
            Transaction::write_read(0x22, vec![2], vec![0xa7]),
            Transaction::write_read(0x22, vec![2], vec![0xa7]),
            Transaction::write(0x22, vec![2, 0xa5]),
            Transaction::write_read(0x22, vec![6], vec![0xfd]),
            Transaction::write_read(0x22, vec![2], vec![0xa5]),
        ]);
        routing_gpio(&mut bus).unwrap();
        bus.done();
    }

    #[test]
    fn redriver_failure_still_deselects_the_switch() {
        let mut bus = Mock::new(&[
            Transaction::write(0x72, vec![1]),
            Transaction::write(0x58, vec![6, 0x18]).with_error(ErrorKind::Other),
            Transaction::write(0x72, vec![0]),
        ]);
        assert_eq!(tune_redrivers(&mut bus, 2), Err(BoardError::Device));
        bus.done();
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
