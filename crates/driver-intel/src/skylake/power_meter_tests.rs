// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::skylake::{registers::testing::image, training_state::OdtPowerSaving};
use tock_registers::interfaces::{Readable, Writeable};

#[test]
fn meminit_power_saving_meter_update_first_second_zero_overflow_high_current_reserved_bits() {
    let image = image();
    let r = image.registers();
    let mut s = OdtPowerSaving::default();
    r.odt_power_saving.set(0xab987654);
    record_power_saving(
        r,
        &mut s,
        SystemPower {
            read: 65535,
            write: 256,
            command: 0,
        },
    );
    assert_eq!(r.odt_power_saving.get(), 0xab987654);
    record_power_saving(
        r,
        &mut s,
        SystemPower {
            read: 0,
            write: 128,
            command: 0,
        },
    );
    assert_eq!(r.odt_power_saving.get(), 0xab0080ff);
    assert_eq!(s.base_saving_rd, 65535);
    assert_eq!(s.meminit_saving_rd, 0);
    record_power_saving(
        r,
        &mut s,
        SystemPower {
            read: 65535,
            write: 65535,
            command: 0,
        },
    );
    assert_eq!(r.odt_power_saving.get(), 0xab000000);
    let mut zero = OdtPowerSaving::default();
    record_power_saving(
        r,
        &mut zero,
        SystemPower {
            read: 0,
            write: 0,
            command: 0,
        },
    );
    record_power_saving(
        r,
        &mut zero,
        SystemPower {
            read: 1,
            write: 65535,
            command: 0,
        },
    );
    assert_eq!(r.odt_power_saving.get(), 0xab000000);
    assert_eq!(saving_fraction(65535, 1), 255);
    assert_eq!(saving_fraction(1, 0), 255);
    assert_eq!(saving_fraction(0, 0), 0);
    assert_eq!(saving_fraction(100, 101), 0);
}
