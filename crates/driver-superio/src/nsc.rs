//! Common PC87382/PC87384 external-clock request used by ThinkPad docks.
//!
//! Register 29h is chip-specific: PC87392 uses a different request/status.
//! The complete A0h request and ready bit are established by coreboot's
//! X60/X61/T60 dock paths. This is not a decoded general clock configuration
//! API, and deliberately does not infer support for other NSC controllers.

use fstart_core::pio::PioRegister;
use fstart_core::services::device::DeviceError;
use tock_registers::{
    fields::{Field, FieldValue},
    interfaces::{ReadWriteable, Readable, Writeable},
};

/// Bank-0 data latch shared by PC87382 and PC87384. Caller owns decode,
/// direction, mux and serialization; this handle does not allocate resources.
pub struct GpioPort0 {
    base: u16,
}

fn level_fields(levels: &[(u8, bool)]) -> Result<Option<FieldValue<u8, ()>>, DeviceError> {
    if levels
        .iter()
        .enumerate()
        .any(|(i, &(pin, _))| pin > 7 || levels.iter().take(i).any(|&(other, _)| pin == other))
    {
        return Err(DeviceError::ConfigError);
    }
    Ok(levels
        .iter()
        .map(|&(pin, high)| Field::<u8, ()>::new(1, usize::from(pin)).val(u8::from(high)))
        .reduce(|a, b| a + b))
}

impl GpioPort0 {
    pub(crate) fn new(base: u16) -> Result<Self, DeviceError> {
        if base < 0x100 || base & 15 != 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { base })
    }

    /// Change physical GP00–07 latches in one update, preserving siblings.
    /// Invalid/duplicate pins fail before I/O; an empty policy does no I/O.
    pub fn set_levels(&self, levels: &[(u8, bool)]) -> Result<(), DeviceError> {
        if let Some(fields) = level_fields(levels)? {
            PioRegister::<u8>::new(self.base).modify(fields);
        }
        Ok(())
    }

    pub fn set_level(&self, pin: u8, high: bool) -> Result<(), DeviceError> {
        self.set_levels(&[(pin, high)])
    }
}

tock_registers::register_bitfields![u8,
    pub(crate) CLOCK_CONTROL [READY OFFSET(4) NUMBITS(1) []]
];

pub(crate) fn start_external_14mhz_clock(
    control: &(
         impl Readable<T = u8, R = CLOCK_CONTROL::Register>
         + Writeable<T = u8, R = CLOCK_CONTROL::Register>
     ),
    timeout_ms: u32,
    mut delay_us: impl FnMut(u32),
) -> Result<(), DeviceError> {
    // Full reference clock-controller request, not a field-preserving update.
    control.set(0xa0);
    for _ in 0..timeout_ms {
        if control.is_set(CLOCK_CONTROL::READY) {
            return Ok(());
        }
        delay_us(1000);
    }
    if control.is_set(CLOCK_CONTROL::READY) {
        Ok(())
    } else {
        Err(DeviceError::InitFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use tock_registers::registers::InMemoryRegister;

    #[test]
    fn bank0_updates_preserve_siblings_and_validate_before_io() {
        assert_eq!(
            level_fields(&[(0, false), (1, false)])
                .unwrap()
                .unwrap()
                .modify(0xff),
            0xfc
        );
        assert_eq!(
            level_fields(&[(0, true), (1, true), (3, true)])
                .unwrap()
                .unwrap()
                .modify(0xa4),
            0xaf
        );
        assert!(GpioPort0::new(0x1681).is_err());
        let gpio = GpioPort0::new(0x1680).unwrap();
        assert!(gpio.set_levels(&[]).is_ok());
        assert!(gpio.set_levels(&[(8, true)]).is_err());
        assert!(gpio.set_levels(&[(0, true), (0, false)]).is_err());
    }

    #[test]
    fn clock_polling_has_a_bounded_deadline_and_final_ready_sample() {
        let control = InMemoryRegister::<u8, CLOCK_CONTROL::Register>::new(0);
        let waits = Cell::new(0);
        let result = start_external_14mhz_clock(&control, 2, |us| {
            assert_eq!(us, 1000);
            waits.set(waits.get() + 1);
            if waits.get() == 2 {
                control.set(0x10);
            }
        });
        assert!(result.is_ok());
        assert_eq!(waits.get(), 2);
        assert!(start_external_14mhz_clock(&InMemoryRegister::new(0), 0, |_| panic!()).is_err());
    }
}
