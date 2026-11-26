// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native client package limits, after BIOS_RESET_CPL and before MP startup.
//! No RAPL disabling, OC mailbox, PSys/PL3/PL4 or server PL2 time programming.
//! Limit readback is not power telemetry, cooling approval or thermal proof.
//! Source: common/block/power_limit/power_limit.c, SHA256
//! a7c05eefc8fa9c5e9e27ebd5985fa80a5d9ffc169d7b69a5e4d640d244ffb44d.
//! PAIR: common/block/systemagent/systemagent.c, SHA256
//! 7367a8a814eef9659ac1b790fc47e72a9871334764fb04358f2a5b63976d2c2d.
//! BIOS completion: common/block/systemagent/systemagent_early.c, SHA256
//! 2b87db45eabbe90214abc4c37605e36b2710a684604dacade58fa87774956ab1.
//! Ordering: skylake/systemagent.c, SHA256
//! 4a8fd0f0f8bc4900077848a9aa0fb9b68e2428f4884264cd4bcefb718d96d683.
use super::registers::{
    BiosResetComplete, MchPowerLimit, MchRegisters, PowerAwareInterrupts, SaPmControl,
};
use core::num::NonZeroU16;
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, PLATFORM_INFO, PlatformInfo};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    pub(crate) Units [
        POWER OFFSET(0) NUMBITS(4) [],
        ENERGY OFFSET(8) NUMBITS(5) [],
        TIME OFFSET(16) NUMBITS(4) []
    ],
    pub(crate) Sku [
        TDP OFFSET(0) NUMBITS(15) [],
        MIN OFFSET(16) NUMBITS(15) [],
        MAX OFFSET(32) NUMBITS(15) [],
        WINDOW OFFSET(48) NUMBITS(7) []
    ],
    PackageLimit [
        PL1 OFFSET(0) NUMBITS(15) [],
        ENABLE1 OFFSET(15) NUMBITS(1) [],
        CLAMP1 OFFSET(16) NUMBITS(1) [],
        WINDOW1 OFFSET(17) NUMBITS(7) [],
        PL2 OFFSET(32) NUMBITS(15) [],
        ENABLE2 OFFSET(47) NUMBITS(1) [],
        CLAMP2 OFFSET(48) NUMBITS(1) [],
        LOCK OFFSET(63) NUMBITS(1) []
    ],
    DramLimit [ LOCK OFFSET(31) NUMBITS(1) [] ],
    pub(crate) Nominal [ RATIO OFFSET(0) NUMBITS(8) [] ],
    Activation [
        RATIO OFFSET(0) NUMBITS(8) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ]
];
pub(crate) const UNITS: Msr<Units::Register> = Msr::new(0x606);
pub(crate) const SKU: Msr<Sku::Register> = Msr::new(0x614);
const PACKAGE: Msr<PackageLimit::Register> = Msr::new(0x610);
const DRAM: Msr<DramLimit::Register> = Msr::new(0x618);
pub(crate) const NOMINAL: Msr<Nominal::Register> = Msr::new(0x648);
const ACTIVATION: Msr<Activation::Register> = Msr::new(0x64c);

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub pl1_watts: Option<NonZeroU16>,
    pub pl2_watts: Option<NonZeroU16>,
    pub pl1_window_seconds: NonZeroU16,
}
impl Policy {
    /// Source defaults, not approval of any board's cooling or VR design.
    pub const NATIVE: Self = Self {
        pl1_watts: None,
        pl2_watts: None,
        pl1_window_seconds: NonZeroU16::new(28).unwrap(),
    };
    pub fn validate(self) -> Result<(), Error> {
        if self.pl1_window_seconds.get() > 128 {
            return Err(Error::Policy);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Unavailable,
    NotProgrammable,
    InvalidSku,
    Overflow,
    Locked,
    UnsupportedDramLimits,
    ResetComplete,
    Readback,
}

/// No success/completion flag: only a plan derived from actual SKU registers.
pub(crate) struct Plan {
    limits: Option<(u16, u16, u8)>,
    nominal_ratio: Option<u8>,
}

fn window_quarter_ticks(encoded: u8) -> u64 {
    (4 + u64::from(encoded >> 5)) << (encoded & 31)
}

impl Plan {
    pub(crate) fn prepare(
        mch: &MchRegisters,
        cpu: &impl MsrAccess,
        policy: Policy,
    ) -> Result<Self, Error> {
        policy.validate()?;
        let info = cpu.reg(PLATFORM_INFO).extract();
        if info.get() == u64::MAX {
            return Err(Error::Unavailable);
        }
        if !info.is_set(PlatformInfo::POWER_LIMITS_WRITABLE) {
            return if policy.pl1_watts.is_some() || policy.pl2_watts.is_some() {
                Err(Error::NotProgrammable)
            } else {
                Ok(Self {
                    limits: None,
                    nominal_ratio: None,
                })
            };
        }
        let units = cpu.reg(UNITS).extract();
        let sku = cpu.reg(SKU).extract();
        let (pl1, pl2, window) = compute_limits(units, sku, policy)?;
        let nominal_ratio = match info.read(PlatformInfo::CONFIG_TDP_LEVELS) {
            0 => None,
            1 | 2 => {
                let nominal = cpu.reg(NOMINAL).extract();
                let ratio = nominal.read(Nominal::RATIO) as u8;
                if nominal.get() == u64::MAX || ratio == 0 || ratio == 0xff {
                    return Err(Error::InvalidSku);
                }
                Some(ratio)
            }
            _ => return Err(Error::InvalidSku),
        };
        let plan = Self {
            limits: Some((pl1, pl2, window)),
            nominal_ratio,
        };
        plan.preflight(mch, cpu)?;
        Ok(plan)
    }

    fn preflight(&self, mch: &MchRegisters, cpu: &impl MsrAccess) -> Result<(), Error> {
        if self.limits.is_none() {
            return Ok(());
        }
        let package = cpu.reg(PACKAGE).extract();
        let dram = cpu.reg(DRAM).extract();
        let high = mch.package_power_limit[1].extract();
        if package.get() == u64::MAX
            || dram.get() == u64::MAX
            || high.get() == u32::MAX
            || mch.package_power_limit[0].get() == u32::MAX
        {
            return Err(Error::Unavailable);
        }
        if package.is_set(PackageLimit::LOCK)
            || high.is_set(MchPowerLimit::LOCK)
            || dram.is_set(DramLimit::LOCK)
        {
            return Err(Error::Locked);
        }
        // The actual cold activation producer disables both DDR MMIO words.
        // Active DDR power limits are not an implemented policy in this port.
        if mch.rapl_limit.iter().any(|word| word.get() != 0) {
            return Err(Error::UnsupportedDramLimits);
        }
        if self.nominal_ratio.is_some() {
            let activation = cpu.reg(ACTIVATION).extract();
            if activation.get() == u64::MAX {
                return Err(Error::Unavailable);
            }
            if activation.is_set(Activation::LOCK) {
                return Err(Error::Locked);
            }
        }
        Ok(())
    }

    /// Graphics PM runs after reset completion and before this source wait.
    pub(crate) fn install(
        self,
        mch: &MchRegisters,
        cpu: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let reset = mch.bios_reset_complete.extract();
        if reset.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
        if !reset.matches_all(BiosResetComplete::MEMORY_AND_PM.val(3)) {
            return Err(Error::ResetComplete);
        }
        delay.delay_ms(1);
        // PCU reset/graphics work may have changed locks since preparation.
        self.preflight(mch, cpu)?;
        let Some((pl1, pl2, window)) = self.limits else {
            return Ok(());
        };
        let fields = PackageLimit::PL1.val(u64::from(pl1))
            + PackageLimit::ENABLE1::SET
            + PackageLimit::CLAMP1::SET
            + PackageLimit::WINDOW1.val(u64::from(window))
            + PackageLimit::PL2.val(u64::from(pl2))
            + PackageLimit::ENABLE2::SET
            + PackageLimit::CLAMP2::SET;
        cpu.reg(PACKAGE).modify(fields);
        if !cpu.reg(PACKAGE).matches_all(fields) {
            return Err(Error::Readback);
        }
        let mmio_fields = [
            MchPowerLimit::POWER.val(u32::from(pl1))
                + MchPowerLimit::ENABLE::SET
                + MchPowerLimit::CLAMP::SET
                + MchPowerLimit::WINDOW.val(u32::from(window)),
            MchPowerLimit::POWER.val(u32::from(pl2))
                + MchPowerLimit::ENABLE::SET
                + MchPowerLimit::CLAMP::SET,
        ];
        for (word, fields) in mch.package_power_limit.iter().zip(mmio_fields) {
            word.modify(fields);
            if !word.matches_all(fields) {
                return Err(Error::Readback);
            }
        }
        cpu.reg(DRAM).set(0); // Copy the observed, required-zero DDR MMIO limit.
        if cpu.reg(DRAM).get() != 0 {
            return Err(Error::Readback);
        }
        if let Some(ratio) = self.nominal_ratio {
            let fields = Activation::RATIO.val(u64::from(ratio));
            cpu.reg(ACTIVATION).modify(fields);
            if !cpu.reg(ACTIVATION).matches_all(fields) {
                return Err(Error::Readback);
            }
        }
        self.preflight(mch, cpu)?;
        if !cpu.reg(PACKAGE).matches_all(fields)
            || mch
                .package_power_limit
                .iter()
                .zip(mmio_fields)
                .any(|(word, fields)| !word.matches_all(fields))
        {
            return Err(Error::Readback);
        }
        Ok(())
    }
}

/// Source PAIR routing is established before post-memory link initialization.
pub(crate) fn configure_interrupt_routing(mch: &MchRegisters) -> Result<(), Error> {
    let pair = &mch.power_aware_interrupts;
    if pair.get() == u8::MAX {
        return Err(Error::Unavailable);
    }
    pair.modify(PowerAwareInterrupts::PRIORITY::Fixed);
    if !pair.matches_all(PowerAwareInterrupts::PRIORITY::Fixed) {
        return Err(Error::Readback);
    }
    Ok(())
}

/// Publish the real SA initialization boundary, preserving PEG/other bits.
/// This is not MC training completion or permission to import arbitrary RAM.
pub(crate) fn complete_bios_reset(mch: &MchRegisters) -> Result<(), Error> {
    let current = mch.bios_reset_complete.get();
    if current == u32::MAX {
        return Err(Error::Unavailable);
    }
    let completion = BiosResetComplete::MEMORY_AND_PM.val(3);
    if mch
        .bios_reset_complete
        .read(BiosResetComplete::MEMORY_AND_PM)
        != 0
    {
        return Err(Error::ResetComplete);
    }
    if mch.power_aware_interrupts.get() == u8::MAX
        || !mch
            .power_aware_interrupts
            .matches_all(PowerAwareInterrupts::PRIORITY::Fixed)
        || mch.sa_pm_control.get() == u32::MAX
    {
        return Err(Error::Readback);
    }
    mch.sa_pm_control.modify(SaPmControl::PM_SYNC.val(3));
    if !mch.sa_pm_control.matches_all(SaPmControl::PM_SYNC.val(3)) {
        return Err(Error::Readback);
    }
    mch.bios_reset_complete.modify(completion);
    if !mch.bios_reset_complete.matches_all(completion) {
        return Err(Error::Readback);
    }
    Ok(())
}

fn compute_limits(
    units: tock_registers::LocalRegisterCopy<u64, Units::Register>,
    sku: tock_registers::LocalRegisterCopy<u64, Sku::Register>,
    policy: Policy,
) -> Result<(u16, u16, u8), Error> {
    let unit_fields =
        Units::POWER.val(15).value | Units::ENERGY.val(31).value | Units::TIME.val(15).value;
    if units.get() & !unit_fields != 0 || sku.get() == u64::MAX {
        return Err(Error::Unavailable);
    }
    let tdp = sku.read(Sku::TDP);
    let minimum = sku.read(Sku::MIN);
    let maximum = sku.read(Sku::MAX);
    if tdp == 0 || maximum != 0 && minimum > maximum {
        return Err(Error::InvalidSku);
    }
    let power_unit = 1u64 << units.read(Units::POWER);
    let mut pl1 = policy
        .pl1_watts
        .map_or(tdp, |watts| u64::from(watts.get()) * power_unit);
    if minimum != 0 {
        pl1 = pl1.max(minimum);
    }
    if maximum != 0 {
        pl1 = pl1.min(maximum);
    }
    let pl2 = policy
        .pl2_watts
        .map_or(tdp * 125 / 100, |watts| u64::from(watts.get()) * power_unit)
        .max(pl1);
    // Never turn an excessive watt override into a small limit by masking.
    if pl1 == 0 || pl1 > 0x7fff || pl2 > 0x7fff {
        return Err(Error::Overflow);
    }
    // Match the source's longer-SKU-window preference, using the ACTUAL
    // time unit and all seven-bit encodings, not a sparse unchecked table.
    let target = (u64::from(policy.pl1_window_seconds.get()) << (units.read(Units::TIME) + 2))
        .max(window_quarter_ticks(sku.read(Sku::WINDOW) as u8));
    let window = (0..128u8)
        .filter(|value| window_quarter_ticks(*value) <= target)
        .max_by_key(|value| window_quarter_ticks(*value))
        .ok_or(Error::InvalidSku)?;
    Ok((pl1 as u16, pl2 as u16, window))
}

#[cfg(test)]
mod tests;
