// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso interrupt routing, matching the native NBIO bridge table.
use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, prt};
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
        duty_width: 0,
        pm2_cnt_offset: None,
    },
    bridges: &prt::BRIDGES_RV,
    fch_intx_gsi: Some([16, 17, 18, 19]),
    wake: None,
    xhci: None,
    // Without ACPI resume there is no S3.
    sleep_states: &[(0, 0), (1, 1), (4, 4), (5, S5_SLEEP_TYPE)],
    // One die: no SLIT.
    socket_distance: 10,
};

/// NameSeg of FCH I2C controller `index`.
pub fn fch_i2c_name(index: u8) -> alloc::string::String {
    format!("I2C{index:X}")
}

/// The FCH GPIO controller and the board's I2C controllers with the
/// devices behind them, for `\_SB`.
pub fn devices(buses: &[super::I2cBus], emmc_enabled: bool) -> Result<Vec<u8>, AmlError> {
    use fstart_acpi::devices::i2c::{self, DesignWareSpeed, GpioInterrupt, I2cDevice};
    use fstart_driver_amd::{
        fch::{self, i2c::Speed, sandstone::legacy},
        soc::Soc,
    };
    let irq = |source| legacy::internal_irq(source).ok_or(AmlError::InvalidPath);
    // Picasso's four GPIO banks (coreboot's mmio.asl).
    let mut aml = fch::aml::gpio(irq(legacy::source::GPIO)?, 0x400)?;
    if emmc_enabled {
        aml.extend(emmc_card()?);
    }
    for bus in buses {
        let index = bus.controller;
        let (Some(&base), Some(&source)) =
            (Picasso::I2C.get(index), legacy::source::I2C.get(index))
        else {
            return Err(AmlError::InvalidPath);
        };
        let index = index as u8;
        let path = format!("\\_SB.{}", fch_i2c_name(index));
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
                    controller: fch::aml::GPIO_PATH,
                    pin: pin.into(),
                    edge: false,
                    active_low,
                },
                super::GpioInterrupt::Edge { pin, falling } => GpioInterrupt {
                    controller: fch::aml::GPIO_PATH,
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
        aml.extend(fch::aml::i2c(
            &fch_i2c_name(index),
            index,
            base as u32,
            irq(source)?,
            &body,
        )?);
    }
    Ok(aml)
}

/// The same fixed-address host inventory is consumed by ACPI and CrabEFI.
/// Its IRQ is level-low, unlike the generic device's level-high default.
/// Losing this inventory when fixing AML would also lose UEFI eMMC boot.
pub fn emmc() -> Result<fstart_core::acpi::AcpiGenericDevice, AmlError> {
    use fstart_core::acpi::{AcpiGenericDevice, AcpiInterruptFlags, AcpiResource};
    use fstart_driver_amd::fch::sandstone::legacy;
    Ok(AcpiGenericDevice {
        name: fstart_core::hstr("MMC0"),
        hid: fstart_core::hstr("AMDI0040"),
        resources: [AcpiResource::Mmio {
            base: 0xfedd_5000,
            size: 0x1000,
        }]
        .into_iter()
        .collect(),
        gsiv: Some(
            legacy::internal_irq(legacy::source::EMMC)
                .ok_or(AmlError::InvalidPath)?
                .into(),
        ),
        interrupt_flags: AcpiInterruptFlags {
            active_low: true,
            ..Default::default()
        },
    })
}

fn emmc_card() -> Result<Vec<u8>, AmlError> {
    use fstart_acpi::aml_linker::scope_vec;
    use fstart_acpi_macros::acpi_dsl;
    let body = acpi_dsl! {
        Device("CARD") {
            Name("_ADR", 8u32);
            Method("_RMV", 0, NotSerialized) { Return(0u32); }
        }
    };
    scope_vec("\\_SB.MMC0", &Vec::<u8>::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emmc_has_the_routed_level_low_irq_and_a_nonremovable_card() {
        let device = emmc().unwrap();
        let mut aml = fstart_acpi::devices::GenericAcpi {
            name: device.name.as_str(),
            hid: device.hid.as_str(),
            resources: &device.resources,
            gsiv: device.gsiv,
            interrupt_flags: device.interrupt_flags,
        }
        .dsdt_aml();
        aml.extend(emmc_card().unwrap());
        // Extended Interrupt: consumer, level, active-low, exclusive, GSI 5.
        assert!(aml.windows(9).any(|w| w == [0x89, 6, 0, 5, 1, 5, 0, 0, 0]));
        assert!(aml.windows(8).any(|w| w == b"AMDI0040"));
        assert!(aml.windows(4).any(|w| w == b"CARD"));
        assert!(aml.windows(4).any(|w| w == b"_RMV"));
    }

    #[test]
    fn disabled_emmc_is_not_described() {
        let disabled = devices(&[], false).unwrap();
        let enabled = devices(&[], true).unwrap();
        assert!(!disabled.windows(4).any(|w| w == b"MMC0"));
        assert!(enabled.windows(4).any(|w| w == b"MMC0"));
    }
}
