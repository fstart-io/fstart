//! I2C devices and DesignWare I2C controllers in `\_SB`: what an OS needs
//! to bind the drivers of devices without a discoverable bus.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use acpi_tables::Aml;
use acpi_tables::aml::{Name, Package, ResourceTemplate};
use fstart_acpi_macros::acpi_dsl;

use crate::AmlError;
use crate::aml_linker::device_vec;
use crate::descriptors::gpio::{GpioInt, GpioIntMode, GpioIntPolarity, GpioPinConfig};
use crate::descriptors::i2c::I2cSerialBus;

/// An interrupt a device signals on a pad of a GPIO controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioInterrupt<'a> {
    /// Path of the GPIO controller, e.g. `"\\_SB.GPIO"`.
    pub controller: &'a str,
    pub pin: u16,
    /// Edge- rather than level-triggered.
    pub edge: bool,
    /// Low level or falling edge.
    pub active_low: bool,
}

/// An I2C device: `_HID`, `_CID`, `_UID`, its bus connection and interrupt
/// in `_CRS` and, for HID over I2C, the HID descriptor register in `_DSM`.
#[derive(Debug, Clone, Copy)]
pub struct I2cDevice<'a> {
    /// ACPI NameSeg, e.g. `"TPAD"`.
    pub name: &'a str,
    /// Hardware ID, then compatible IDs.
    pub ids: &'a [&'a str],
    pub uid: u32,
    /// Path of the I2C controller, e.g. `"\\_SB.I2C2"`.
    pub bus: &'a str,
    pub address: u16,
    pub speed_hz: u32,
    pub interrupt: Option<GpioInterrupt<'a>>,
    /// HID over I2C: the register of the HID descriptor.
    pub hid_descriptor: Option<u16>,
}

impl I2cDevice<'_> {
    pub fn dsdt_aml(&self) -> Result<Vec<u8>, AmlError> {
        let (hid, cids) = self.ids.split_first().ok_or(AmlError::InvalidPath)?;
        let mut body = Vec::new();
        Name::new("_HID".into(), &String::from(*hid)).to_aml_bytes(&mut body);
        let cids: Vec<String> = cids.iter().map(|&id| id.into()).collect();
        match cids.as_slice() {
            [] => {}
            [cid] => Name::new("_CID".into(), cid).to_aml_bytes(&mut body),
            _ => {
                let elements: Vec<&dyn Aml> = cids.iter().map(|cid| cid as &dyn Aml).collect();
                Name::new("_CID".into(), &Package::new(elements)).to_aml_bytes(&mut body);
            }
        }
        Name::new("_UID".into(), &self.uid).to_aml_bytes(&mut body);
        let connection = I2cSerialBus {
            resource_source: self.bus,
            slave_address: self.address,
            connection_speed: self.speed_hz,
            address_10bit: false,
            consumer: true,
        };
        let pins = self.interrupt.map(|interrupt| [interrupt.pin]);
        let interrupt = self
            .interrupt
            .zip(pins.as_ref())
            .map(|(interrupt, pins)| GpioInt {
                resource_source: interrupt.controller,
                pins,
                consumer: true,
                mode: if interrupt.edge {
                    GpioIntMode::Edge
                } else {
                    GpioIntMode::Level
                },
                polarity: if interrupt.active_low {
                    GpioIntPolarity::ActiveLow
                } else {
                    GpioIntPolarity::ActiveHigh
                },
                shared: false,
                pin_config: GpioPinConfig::Default,
                debounce: 0,
            });
        let mut resources: Vec<&dyn Aml> = vec![&connection];
        if let Some(interrupt) = &interrupt {
            resources.push(interrupt);
        }
        Name::new("_CRS".into(), &ResourceTemplate::new(resources)).to_aml_bytes(&mut body);
        // The HID-over-I2C `_DSM`: function 1 returns the descriptor register.
        if let Some(register) = self.hid_descriptor {
            let register = u64::from(register);
            acpi_dsl! {
                Method("_DSM", 4, Serialized) {
                    If (Arg0 == ToUUID("3cdff6f7-4267-4555-ad05-b30a3d8938de")) {
                        If (Arg2 == 0u8) { Return(Buffer(0x03u8)); }
                        If (Arg2 == 1u8) { Return(#{word register}); }
                    }
                    Return(Buffer(0x00u8));
                }
            }
            .emit(&mut body)?;
        }
        device_vec(self.name, &body)
    }
}

/// Bus speed of a DesignWare controller, naming the method with its timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignWareSpeed {
    /// 100 kHz: `SSCN`.
    Standard,
    /// 400 kHz: `FMCN`.
    Fast,
}

/// The SCL high and low counts and the SDA hold time of a DesignWare I2C
/// controller at `speed`, in controller clocks: the `SSCN`/`FMCN` package
/// an OS programs the controller with instead of deriving its own.
pub fn designware_timing_aml(
    speed: DesignWareSpeed,
    scl_high: u16,
    scl_low: u16,
    sda_hold: u16,
) -> Result<Vec<u8>, AmlError> {
    let (high, low, hold) = (u64::from(scl_high), u64::from(scl_low), u64::from(sda_hold));
    let mut body = Vec::new();
    match speed {
        DesignWareSpeed::Standard => acpi_dsl! {
            Name("SSCN", Package(#{word high}, #{word low}, #{word hold}));
        }
        .emit(&mut body)?,
        DesignWareSpeed::Fast => acpi_dsl! {
            Name("FMCN", Package(#{word high}, #{word low}, #{word hold}));
        }
        .emit(&mut body)?,
    }
    Ok(body)
}
