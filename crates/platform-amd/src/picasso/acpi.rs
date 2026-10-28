// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso interrupt routing, matching the native NBIO bridge table.
use crate::acpi::{ABCD, CDAB, Error, Route, SocAcpi};
use alloc::{format, vec::Vec};
use fstart_acpi::AmlError;
use fstart_acpi::{
    fadt::{Flags, PmProfile},
    platform::{X86Fadt, x86::IAPC_LEGACY_DEVICES},
};
use fstart_driver_amd::soc::picasso::Picasso;

pub const ACPI: SocAcpi = SocAcpi {
    ports: fstart_driver_amd::fch::sandstone::ACPI_PORTS,
    fadt: X86Fadt {
        pm_profile: PmProfile::Mobile,
        iapc_boot_arch: IAPC_LEGACY_DEVICES,
        flags: Flags::Wbinvd as u32
            | Flags::ProcC1 as u32
            | Flags::SlpButton as u32
            | Flags::RtcS4 as u32
            | Flags::TmrValExt as u32
            | Flags::PciExpWak as u32
            | Flags::UsePlatformClock as u32
            | Flags::S4RtcStsValid as u32,
        p_lvl2_lat: 101,
        p_lvl3_lat: 1001,
        duty_offset: 0,
    },
    bridge_routes: &[
        Route::new(1, 1, 0, ABCD, 0x10),
        Route::new(1, 2, 1, ABCD, 0x11),
        Route::new(1, 3, 2, ABCD, 0x12),
        Route::new(1, 4, 3, ABCD, 0x13),
        Route::new(1, 5, 4, ABCD, 0x10),
        Route::new(1, 6, 5, ABCD, 0x11),
        Route::new(1, 7, 6, ABCD, 0x12),
        Route::new(8, 1, 7, ABCD, 0x13),
        Route::new(8, 2, 7, CDAB, 0x0c),
    ],
    fch_intx_gsi: [16, 17, 18, 19],
};

/// The FCH GPIO controller and the board's I2C controllers with the
/// devices behind them, for `\_SB`.
pub fn devices(buses: &[super::I2cBus]) -> Result<Vec<u8>, Error> {
    use fstart_acpi::devices::i2c::{self, DesignWareSpeed, GpioInterrupt, I2cDevice};
    use fstart_driver_amd::{
        fch::{i2c::Speed, sandstone::legacy},
        soc::Soc,
    };
    let irq = |source| legacy::internal_irq(source).ok_or(Error::Aml(AmlError::InvalidPath));
    let mut aml = crate::acpi::fch_gpio(irq(legacy::source::GPIO)?)?;
    for bus in buses {
        let index = bus.controller;
        let (Some(&base), Some(&source)) =
            (Picasso::I2C.get(index), legacy::source::I2C.get(index))
        else {
            return Err(Error::Aml(AmlError::InvalidPath));
        };
        let index = index as u8;
        let path = format!("\\_SB.{}", crate::acpi::fch_i2c_name(index));
        let timing = &bus.timing;
        let (speed, speed_hz) = match timing.speed {
            Speed::Standard => (DesignWareSpeed::Standard, 100_000),
            Speed::Fast => (DesignWareSpeed::Fast, 400_000),
        };
        let mut body = i2c::designware_timing_aml(
            speed,
            timing.scl_high,
            timing.scl_low,
            timing.sda_hold.unwrap_or(0),
        )?;
        for (n, device) in bus.devices.iter().enumerate() {
            let interrupt = device.interrupt.map(|interrupt| match interrupt {
                super::GpioInterrupt::Level { pin, active_low } => GpioInterrupt {
                    controller: crate::acpi::FCH_GPIO_PATH,
                    pin: pin.into(),
                    edge: false,
                    active_low,
                },
                super::GpioInterrupt::Edge { pin, falling } => GpioInterrupt {
                    controller: crate::acpi::FCH_GPIO_PATH,
                    pin: pin.into(),
                    edge: true,
                    active_low: falling,
                },
            });
            body.extend(
                I2cDevice {
                    name: &format!("D{:02X}{n:X}", device.address),
                    ids: device.ids,
                    uid: n as u32,
                    bus: &path,
                    address: device.address.into(),
                    speed_hz,
                    interrupt,
                    hid_descriptor: device.hid_descriptor_register,
                }
                .dsdt_aml()?,
            );
        }
        aml.extend(crate::acpi::fch_i2c(
            index,
            base as u32,
            irq(source)?,
            &body,
        )?);
    }
    Ok(aml)
}
