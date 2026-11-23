// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCODE memory-PLL request from native/mc_reg.c:frequency_mts,
//! frequency_ratio and meminit_set_frequency. RCOMP completion is required
//! before touching MCDECS or DDRIO registers.

use super::{
    Error,
    registers::{BiosData, BiosRequest, Compensation, MchRegisters},
    timings::{Clock, ReferenceClock, select_clock},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pll {
    pub ratio: u8,
    pub reference: ReferenceClock,
    pub odd_ratio: bool,
    pub data_rate_mts: u16,
}

fn requested(clock: Clock) -> Result<Pll, Error> {
    if clock.reference != ReferenceClock::Mhz133 {
        return Err(Error::InvalidTiming);
    }
    // Normalize the truncated DCLK to the PLL's 133.333 MT/s bins.
    let n = (u64::from(clock.clock_khz()) * 3 + 100000) / 200000;
    let mts = (n * 400 + 1) / 3;
    let reference = match clock.reference {
        ReferenceClock::Mhz133 => 266667,
        ReferenceClock::Mhz100 => 200000,
    };
    let ratio = (mts * 1_000_000 / reference + 500) / 1000;
    if !(3..=15).contains(&ratio) {
        return Err(Error::InvalidTiming);
    }
    Ok(Pll {
        ratio: ratio as u8,
        reference: clock.reference,
        odd_ratio: clock.odd_ratio,
        data_rate_mts: mts as u16,
    })
}

/// Real selected/accepted PLL QCLK for late cold services. Reject malformed
/// positive periods, a busy request, or stale/inconsistent hardware frequency.
pub(crate) fn qclk_ps(registers: &MchRegisters, clock: Clock) -> Result<u32, Error> {
    validate_running(registers, clock)?;
    let ps = clock.tck_fs / 2000;
    if !(312..=1250).contains(&ps) {
        return Err(Error::InvalidTiming);
    }
    Ok(ps)
}

fn running_matches(registers: &MchRegisters, pll: Pll) -> bool {
    let data = registers.bios_data.extract();
    let req = registers.bios_request.extract();
    !req.is_set(BiosRequest::RUN_BUSY)
        && data.read(BiosData::RATIO) == u32::from(pll.ratio)
        && !data.is_set(BiosData::RATIO_MSB)
        && req.is_set(BiosRequest::REFCLK_100) == (pll.reference == ReferenceClock::Mhz100)
        && data.is_set(BiosData::ODD_RATIO) == pll.odd_ratio
}

/// Read-only acceptance check for late consumers. A selected profile is not
/// evidence that PCODE accepted it; compare the actual PLL before any writes.
pub(crate) fn validate_running(registers: &MchRegisters, clock: Clock) -> Result<Pll, Error> {
    if select_clock(clock.tck_fs, clock.reference)? != clock {
        return Err(Error::InvalidTiming);
    }
    let pll = requested(clock)?;
    if !running_matches(registers, pll) {
        return Err(Error::PllMismatch);
    }
    Ok(pll)
}

/// Cold standard-profile PLL initialization. SA-GV/extended overclock and a
/// warm PLL that disagrees with the selected timings are deliberately rejected.
/// Unlike the source, an already-running PLL is never labeled with the newly
/// requested (but unprogrammed) timing frequency.
///
/// Prerequisites: system-agent BARs enabled, CPU/pre-SPD overrides complete;
/// no DDRIO/MCDECS accesses yet. This does not initialize or train DRAM.
pub fn initialize(
    registers: &MchRegisters,
    clock: Clock,
    delay: &mut impl DelayNs,
) -> Result<Pll, Error> {
    initialize_bounded(registers, clock, delay, 1_000_000)
}
fn initialize_bounded(
    registers: &MchRegisters,
    clock: Clock,
    delay: &mut impl DelayNs,
    polls: u32,
) -> Result<Pll, Error> {
    let pll = requested(clock)?;
    let data = registers.bios_data.extract();
    let req = registers.bios_request.extract();
    if req.is_set(BiosRequest::RUN_BUSY) {
        return Err(Error::PllAlreadyConfigured);
    }
    if data.read(BiosData::RATIO) != 0 {
        if !running_matches(registers, pll) {
            return Err(Error::PllAlreadyConfigured);
        }
    } else {
        registers.bios_request.write(
            BiosRequest::RATIO.val(pll.ratio.into())
                + BiosRequest::REFCLK_100.val((pll.reference == ReferenceClock::Mhz100) as u32)
                + BiosRequest::ODD_RATIO.val(pll.odd_ratio as u32)
                + BiosRequest::RUN_BUSY::SET,
        );
        let mut complete = false;
        for _ in 0..polls {
            if !registers.bios_request.is_set(BiosRequest::RUN_BUSY) {
                complete = true;
                break;
            }
            delay.delay_us(10);
        }
        if !complete {
            return Err(Error::PllTimeout);
        }
        if !running_matches(registers, pll) {
            return Err(Error::PllMismatch);
        }
    }
    for _ in 0..polls {
        if registers.rcomp_timer.is_set(Compensation::DONE) {
            delay.delay_us(10);
            return Ok(pll);
        }
        delay.delay_us(10);
    }
    Err(Error::CompensationTimeout)
}
