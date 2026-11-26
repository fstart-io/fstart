// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional native `additional_steps.c:meminit_energy_perf_gain` power overhead.
//! These are SA-policy estimates, not SPD currents or measured DRAM power.
//! This callable seam does not train/activate memory or certify usable RAM.

use super::{Error, overrides::write_mailbox, phy::PhyPolicy, registers::MchRegisters, spd::Dimms};
use embedded_hal::delay::DelayNs;

// native_init.h:992–993; bit8 selects the IDD3P overhead, not a new opcode.
const SET_EPG_BIOS_POWER_OVERHEAD_0_CMD: u16 = 0x20;
const SET_EPG_BIOS_POWER_OVERHEAD_1_CMD: u16 = 0x120;

/// Only the genuine native standard DDR4 SA-policy defaults are supported.
/// No profile is inferred from DIMM SPD, manufacturer, frequency or telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// romstage_native.c:729–736: IDD3N=26 mA, IDD3P=11 mA, VDD=1200 mV.
    /// Native leaves EPG disabled by default; selecting this is explicit opt-in
    /// to that policy model, not a claim about the installed DIMMs' currents.
    NativeStandardDdr4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    /// None means unavailable/unsupported, never zero-current fallback.
    pub profile: Option<Profile>,
    /// True for any non-disabled SA-GV mode, including fixed low/high.
    /// This bounded standard-profile implementation does not support SA-GV.
    pub sa_gv: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Disabled,
    /// Both commands completed with PCODE status zero. Not a RAM certificate
    /// or a readback/measurement of power. Entries are unsigned 8.8 watts.
    Published {
        overhead_8_8: [u16; 2],
    },
}

/// Preserve native's two integer divisions and uint16_t intermediate bound,
/// rejecting overflow/narrowing instead of wrapping. mA*mV/1000 gives mW;
/// mW*256/1000 gives unsigned 8.8 watts. There is no frequency-dependent term.
fn power_overhead(idd_ma: u16, vdd_mv: u16, dimm_count: u8) -> Result<u16, Error> {
    if idd_ma == 0 || vdd_mv == 0 || !(1..=4).contains(&dimm_count) {
        return Err(Error::InvalidPolicy);
    }
    let milliwatts = u32::from(idd_ma)
        .checked_mul(u32::from(vdd_mv))
        .and_then(|n| n.checked_mul(u32::from(dimm_count)))
        .ok_or(Error::InvalidPolicy)?
        / 1000;
    let milliwatts = u16::try_from(milliwatts).map_err(|_| Error::InvalidPolicy)?;
    let fixed = u32::from(milliwatts)
        .checked_mul(256)
        .ok_or(Error::InvalidPolicy)?
        / 1000;
    u16::try_from(fixed).map_err(|_| Error::InvalidPolicy)
}

/// Report source IDD3N then IDD3P overhead via PCODE commands 0x20 and 0x120.
/// Reuses canonical retained DIMM population, resolved PHY VDD and the existing
/// MCHBAR/PCODE transport. No PLL check is needed: this native model has no
/// clock input or clock-dependent coefficients.
///
/// All enabled policy, population and arithmetic checks precede either write.
/// Unlike native, missing memory and unsupported profiles/SA-GV reject rather
/// than silently skipping enabled EPG. Disabled performs no mailbox access.
///
/// Caller must own mapped MCHBAR and serialize *all* PCODE users for the whole
/// operation. Each command has two 100-ms maximum ready waits (10,000 samples,
/// 10-us delay per busy sample), at most 400 ms for both commands. Any timeout
/// or nonzero completion status stops immediately. A second-command failure
/// leaves overhead 0 published; there is no rollback, automatic retry or replay.
/// A timeout can leave PCODE busy/its publication unknown: caller must not
/// continue the cold flow as if publication succeeded.
pub fn meminit_energy_perf_gain(
    registers: &MchRegisters,
    dimms: &Dimms<'_>,
    phy: &PhyPolicy,
    policy: Policy,
    delay: &mut impl DelayNs,
) -> Result<Outcome, Error> {
    if !policy.enabled {
        return Ok(Outcome::Disabled);
    }
    if policy.sa_gv || phy.vdd_mv != 1200 {
        return Err(Error::InvalidPolicy);
    }
    let currents_ma = match policy.profile {
        Some(Profile::NativeStandardDdr4) => [26, 11],
        None => return Err(Error::InvalidPolicy),
    };
    let count = dimms.iter().flatten().filter(|dimm| dimm.is_some()).count() as u8;
    if count == 0 {
        return Err(Error::NoMemory);
    }
    let data = [
        power_overhead(currents_ma[0], phy.vdd_mv, count)?,
        power_overhead(currents_ma[1], phy.vdd_mv, count)?,
    ];
    write_mailbox(
        registers,
        delay,
        SET_EPG_BIOS_POWER_OVERHEAD_0_CMD,
        u32::from(data[0]),
    )?;
    write_mailbox(
        registers,
        delay,
        SET_EPG_BIOS_POWER_OVERHEAD_1_CMD,
        u32::from(data[1]),
    )?;
    Ok(Outcome::Published { overhead_8_8: data })
}

#[cfg(test)]
#[path = "energy_perf_gain_tests.rs"]
mod tests;
