// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h Raven/Picasso/Pollock register geometry, not Intel-compatible IP.
use super::Soc;
use fstart_driver_amd::{
    dxio::Descriptor,
    fch::{
        emmc, espi,
        gpio::{Bank, Pad},
        sandstone::{preinit::SpiConfig, usb},
    },
    nbio::picasso as nbio,
};

mod init;
#[cfg(test)]
mod tests;
pub use init::{Chipset, FchUp, Runtime, State, Trained};

/// The Raven/Picasso SoC, one die: [`fstart_driver_amd::ccx::picasso`],
/// [`fstart_driver_amd::df::picasso`], [`fstart_driver_amd::nbio::picasso`], [`fstart_driver_amd::dxio::picasso`],
/// [`fstart_driver_amd::smu::v10`], the Sandstone FCH [`fstart_driver_amd::fch::sandstone`] and the
/// ACP 3.x [`fstart_driver_amd::acp`].
pub enum Picasso {}
impl Soc for Picasso {
    const MAX_DIES: usize = 1;
    const SMU: fstart_driver_amd::smu::Mailbox = fstart_driver_amd::smu::v10::MAILBOX;
    const DXIO_SERVICE: u32 = fstart_driver_amd::smu::v10::DXIO_SERVICE;
    /// GPIO bank 0: pins 0-144, no pad 63.
    const GPIO_BANK: &'static Bank = fstart_driver_amd::fch::sandstone::GPIO_BANK;
    const I2C: &'static [usize] = fstart_driver_amd::fch::sandstone::I2C;
    fn ioms_fabric_id(_: u8) -> u8 {
        9
    }
}
/// Microcode for PCO B0/B1 and RV2 A0, the three vendor .bin inputs.
impl fstart_driver_amd::ccx::microcode::Family for Picasso {
    const SIGNATURES: &'static [u32] = &[0x00810f10, 0x00810f80, 0x00820f00];
    const MAX_PATCHES: usize = 3;
}
pub type Die<P> = fstart_driver_amd::df::Die<Picasso, P>;
pub const ECAM: u64 = 0xf800_0000;
pub const ECAM_BUSES: u8 = 64;
pub const UART_BASES: [usize; 4] = [0xfedc9000, 0xfedca000, 0xfedce000, 0xfedcf000];
pub const UART_AOAC: [u8; 4] = [11, 12, 16, 26];
pub const EARLY_AOAC: &[u8] = &[17, 7, 8, 9, 27];

/// A board's Picasso configuration: how the board wires the SoC's blocks.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub fch: FchConfig,
    pub pcie: &'static [Descriptor],
    pub usb: usb::Config,
    /// The eMMC controller, if the board can have eMMC fitted.
    pub emmc: Option<emmc::Config>,
    pub graphics: nbio::gfx::Config,
    /// FCH I2C controllers and their devices, for the UEFI payload to
    /// drive and the OS to find in ACPI.
    pub i2c: &'static [I2cBus],
}

/// An FCH I2C controller and the devices on it.
#[derive(Debug, Clone, Copy)]
pub struct I2cBus {
    pub controller: usize,
    pub timing: fstart_driver_amd::fch::i2c::Timing,
    pub devices: &'static [I2cDevice],
}

/// An I2C device, possibly one of several fitted alternatives.
#[derive(Debug, Clone, Copy)]
pub struct I2cDevice {
    /// ACPI hardware and compatible IDs, which select the payload's driver.
    pub ids: &'static [&'static str],
    pub address: u8,
    pub interrupt: Option<GpioInterrupt>,
    /// HID descriptor register of a HID-over-I2C device.
    pub hid_descriptor_register: Option<u16>,
}

/// A GPIO interrupt as its pad is configured.
#[derive(Debug, Clone, Copy)]
pub enum GpioInterrupt {
    /// Level-triggered, read from the pad.
    Level { pin: u8, active_low: bool },
    /// Edge-triggered, latched in the pad's interrupt status.
    Edge { pin: u8, falling: bool },
}

/// The Sandstone FCH setup before and right after the console.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// The FCH UART of the console (0-3).
    pub console_uart: u8,
    /// SPI ROM read mode and speeds.
    pub spi: SpiConfig,
    /// eSPI link to the EC and its pads.
    pub espi: Option<(espi::Config, &'static [Pad])>,
}
