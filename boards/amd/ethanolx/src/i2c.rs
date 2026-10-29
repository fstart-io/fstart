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

/// Board card paths, not interchangeable redriver tuning presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedriverMode {
    Off,
    Pcie,
    SataEthernet,
}

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
pub fn configure_redrivers(
    timer: &dyn Timer,
    redrivers: [RedriverMode; 4],
) -> Result<(), BoardError> {
    if redrivers.into_iter().all(|mode| mode == RedriverMode::Off) {
        return Ok(());
    }
    let mut pcie = controller(0, timer)?;
    for (index, mode) in redrivers
        .into_iter()
        .enumerate()
        .filter(|(_, mode)| *mode != RedriverMode::Off)
    {
        tune_redrivers(&mut pcie, index as u8, mode)?;
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

fn tune_redrivers<I: I2c>(bus: &mut I, group: u8, mode: RedriverMode) -> Result<(), BoardError> {
    let count = match mode {
        RedriverMode::Off => return Ok(()),
        RedriverMode::Pcie => 4,
        RedriverMode::SataEthernet => 3,
    };
    let address = MuxAddress::Alternative(false, group & 2 != 0, group & 1 != 0);
    let mut mux = Xca9545a::new(bus, address);
    for index in 0..count {
        let chip = 0x58 + index;
        fstart_log::info!(
            "ethanolx: PCIe switch {:#x}, redriver {:#x}",
            0x70 + group,
            chip
        );
        mux.select_channels(1)
            .map_err(|error| failed(0, "PCIe switch", error))?;
        // RXM100CB: four-chip PCIe table ffd72994 versus three-chip
        // SATA/xGbE table ffd70fd4. Full bytes, including the type-1
        // undocumented channel writes in OEM helper ffd67971.
        let settings = if mode == RedriverMode::SataEthernet {
            Settings {
                equalization: u8::from(index == 1),
                output_voltage: if index == 0 { 0 } else { 0xab },
                deemphasis: 0,
                channel_override: Some(2),
            }
        } else {
            Settings {
                equalization: 0,
                output_voltage: 0,
                deemphasis: 0x11,
                channel_override: None,
            }
        };
        let programming = Ds125Br401a::new(&mut mux, chip)
            .configure(settings)
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
        assert_eq!(
            tune_redrivers(&mut bus, 2, RedriverMode::Pcie),
            Err(BoardError::Device)
        );
        bus.done();
    }

    fn sata_transactions() -> Vec<Transaction> {
        [(0x58, 0, 0), (0x59, 1, 0xab), (0x5a, 0, 0xab)]
            .into_iter()
            .flat_map(|(chip, eq, vod)| {
                core::iter::once(Transaction::write(0x71, vec![1]))
                    .chain(core::iter::once(Transaction::write(chip, vec![6, 0x18])))
                    .chain(
                        [
                            ([0x0f, 0x16, 0x1d, 0x24, 0x2c, 0x33, 0x3a, 0x41], eq),
                            ([0x10, 0x17, 0x1e, 0x25, 0x2d, 0x34, 0x3b, 0x42], vod),
                            ([0x11, 0x18, 0x1f, 0x26, 0x2e, 0x35, 0x3c, 0x43], 0),
                            ([0x0d, 0x14, 0x1b, 0x22, 0x2a, 0x31, 0x38, 0x3f], 2),
                        ]
                        .into_iter()
                        .flat_map(move |(registers, value)| {
                            registers.into_iter().map(move |register| {
                                Transaction::write(chip, vec![register, value])
                            })
                        }),
                    )
                    .chain(core::iter::once(Transaction::write(0x71, vec![0])))
            })
            .collect()
    }

    #[test]
    fn sata_card_uses_three_vendor_profiles_and_channel_overrides() {
        let mut bus = Mock::new(&sata_transactions());
        tune_redrivers(&mut bus, 1, RedriverMode::SataEthernet).unwrap();
        bus.done();
    }

    #[test]
    fn failed_sata_override_still_deselects_the_switch() {
        let mut expected = sata_transactions();
        // Switch + enable + 3*8 channel writes precede the first override.
        expected.truncate(26);
        expected.push(Transaction::write(0x58, vec![0x0d, 2]).with_error(ErrorKind::Other));
        expected.push(Transaction::write(0x71, vec![0]));
        let mut bus = Mock::new(&expected);
        assert_eq!(
            tune_redrivers(&mut bus, 1, RedriverMode::SataEthernet),
            Err(BoardError::Device)
        );
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
