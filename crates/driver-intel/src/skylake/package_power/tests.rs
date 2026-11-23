// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::skylake::registers::testing::image;
use core::cell::RefCell;
use fstart_arch::x86::cpu::msr::testing::MsrSpace;

fn cpu() -> RefCell<MsrSpace> {
    MsrSpace::new([
        (
            PLATFORM_INFO.index(),
            (PlatformInfo::POWER_LIMITS_WRITABLE::SET + PlatformInfo::CONFIG_TDP_LEVELS.val(1))
                .value,
        ),
        (
            UNITS.index(),
            (Units::POWER.val(3) + Units::TIME.val(10)).value,
        ),
        (
            SKU.index(),
            (Sku::TDP.val(35 * 8) + Sku::MIN.val(10 * 8) + Sku::MAX.val(45 * 8)).value,
        ),
        (NOMINAL.index(), 24),
    ])
}

#[test]
fn sku_clamps_time_units_and_full_window_encoding_remain_bounded() {
    let image = image();
    let cpu = cpu();
    cpu.borrow_mut().values.insert(
        UNITS.index(),
        (Units::POWER.val(3) + Units::TIME.val(8)).value,
    );
    let policy = Policy {
        pl1_watts: NonZeroU16::new(100),
        pl2_watts: NonZeroU16::new(20),
        ..Policy::NATIVE
    };
    let plan = Plan::prepare(image.registers(), &cpu, policy).unwrap();
    // PL1 is clamped to SKU maximum; PL2 cannot remain below PL1. A different
    // hardware time unit must not accidentally retain the usual 0x6e encoding.
    assert_eq!(plan.limits, Some((45 * 8, 45 * 8, 0x6c)));
    cpu.borrow_mut()
        .values
        .entry(SKU.index())
        .and_modify(|value| *value |= Sku::WINDOW.val(0x7f).value);
    assert_eq!(
        Plan::prepare(image.registers(), &cpu, policy)
            .unwrap()
            .limits,
        Some((45 * 8, 45 * 8, 0x7f))
    );
}
