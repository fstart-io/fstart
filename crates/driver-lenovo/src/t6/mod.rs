//! T60/R60/Z61t family H8/PMH7 and dock policy, from coreboot's T60 family.
//! This is source-level integration, not hardware or thermal-safety validation.

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod dock;

use crate::{
    ec::{Ec, EcPorts},
    h8::{H8, H8Config, H8Resources},
    pmh7::Pmh7,
};
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::services::Southbridge;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    T60,
    R60,
    Z61t,
}

// T6 differs from X6 at event5 (3f rather than 3c) and eventc (3c not ff).
const EVENT_MASKS: [u8; 16] = [
    0, 0, 0xff, 0xff, 0xf4, 0x3f, 0x80, 1, 1, 0xff, 0xff, 0xff, 0x3c, 0xff, 0, 0,
];
static BLUETOOTH_PRESENT: AtomicBool = AtomicBool::new(false);

pub const fn h8_config(ec_gpe: u8) -> H8Config {
    H8Config {
        has_bluetooth: true,
        has_wwan: true,
        has_thinklight: true,
        second_thermal_zone: true,
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            ec_gpe,
            "IBM0068",
        )
    }
}

/// Complete non-PCI firmware footprint: EC/PMH7 plus dock interfaces.
pub fn firmware_resources(h8: &H8Config) -> impl Iterator<Item = fstart_core::acpi::AcpiResource> {
    h8.resources
        .io_resources()
        .into_iter()
        .chain(dock::FIRMWARE_RESOURCES.iter().cloned())
}

pub fn bluetooth_present() -> bool {
    BLUETOOTH_PRESENT.load(Ordering::Relaxed)
}

fn detect_bluetooth(model: Model, southbridge: &impl Southbridge) -> bool {
    model == Model::Z61t || southbridge.gpio_get(7).is_ok_and(|high| !high)
}

pub fn ec_init(
    model: Model,
    southbridge: &impl Southbridge,
    config: &H8Config,
    resume: bool,
) -> bool {
    let bluetooth = detect_bluetooth(model, southbridge);
    BLUETOOTH_PRESENT.store(bluetooth, Ordering::Relaxed);
    let pmh7 = Pmh7::new(config.resources.pmh7_base.raw());
    pmh7.log_identity();
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.touchpad_enable(true);
    pmh7.trackpoint_enable(true);
    H8::new(Ec::new(config.resources.os)).init_legacy_thinkpad(&EVENT_MASKS, bluetooth, resume)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::ServiceError;
    struct Gpio(Result<bool, ServiceError>);
    impl Southbridge for Gpio {
        fn gpio_get(&self, pin: u32) -> Result<bool, ServiceError> {
            assert_eq!(pin, 7);
            self.0
        }
    }
    #[test]
    fn variant_bluetooth_policy_matches_the_reference() {
        for model in [Model::T60, Model::R60] {
            assert!(detect_bluetooth(model, &Gpio(Ok(false))));
            assert!(!detect_bluetooth(model, &Gpio(Ok(true))));
            assert!(!detect_bluetooth(model, &Gpio(Err(ServiceError::IoError))));
        }
        assert!(detect_bluetooth(
            Model::Z61t,
            &Gpio(Err(ServiceError::IoError))
        ));
    }
}
