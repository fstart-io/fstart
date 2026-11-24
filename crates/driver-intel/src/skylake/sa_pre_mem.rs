// SPDX-License-Identifier: MIT OR Apache-2.0
//! Source-order native SA pre-memory composition, never MemoryInit/MemoryReady.
//! sa_pre_mem_native.c:1678–1714, SHA256
//! 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372.
//! Requires actual PMC classification, exclusive stage-owned UC resources and
//! immutable CPU/board/OC-watchdog facts. No reset or privileged transport is
//! executed by this module; supplied standard MSR/HECI endpoints do the I/O.
use super::{
    cpu::{Cpu, Package},
    cse,
    registers::{HdaAudioRouting, SaPreMemControl},
    sa_dmi, sa_graphics, sa_peg,
};
use crate::{
    heci::{Csr, Fifo},
    sunrise_point::{pcr::Pcr, pmc::MemoryBootMode},
};
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::MsrAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub dmi: sa_dmi::Policy,
    pub peg: sa_peg::Policy,
    pub graphics: sa_graphics::Policy,
    /// Actual platform-read OC_WDT_CTL, not an assumed cold-boot value.
    pub oc_watchdog_control: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Peg(sa_peg::Error),
    Dmi(sa_dmi::Error),
    Cse(cse::Error),
    ConflictingPolicy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bclk {
    Skipped(cse::BclkSkip),
    Reply(cse::BclkResponse),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completed {
    pub dmi_speed: Option<u8>,
    /// None is the actual source OPI/package gate, not missing PEG work.
    pub peg: Option<sa_peg::Outcome>,
    pub peg_primary: Option<u8>,
    pub graphics: sa_graphics::Outcome,
    pub bclk: Bclk,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Platform must perform native's nonreturning warm reset. Final HDA and
    /// completion writes have deliberately NOT happened.
    WarmResetRequested(cse::BclkResponse),
    Complete(Completed),
}

/// Only returns Complete after every invoked native constituent completed.
/// The ULT/ULX flow-credit hook is genuinely empty in the source.
#[allow(clippy::too_many_arguments)]
pub fn init<H, M, W, R>(
    resources: &sa_peg::Resources<'_>,
    dmi: &sa_dmi::Dmi<'_>,
    pcr: &Pcr,
    cpu: Cpu,
    mode: MemoryBootMode,
    policy: Policy,
    msrs: &impl MsrAccess,
    cse: &mut cse::Cse<H, M, W, R>,
    scan: &mut sa_graphics::ScanWorkspace,
    delay: &mut impl DelayNs,
) -> Result<Outcome, Error>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    // All immutable policy/resource failures precede even SI scratchpad writes.
    policy.peg.validate().map_err(Error::Peg)?;
    policy.graphics.validate().map_err(Error::Peg)?;
    if policy.graphics.igd_enabled && !policy.peg.devices.igd {
        return Err(Error::ConflictingPolicy);
    }
    resources
        .validate(dmi, cpu, mode, policy.dmi)
        .map_err(Error::Peg)?;
    let sa = resources.sa().map_err(Error::Peg)?;
    let dmi_link = cpu.package == Package::DesktopHalo && sa.device_id() & 6 != 4;
    if dmi_link {
        resources
            .validate_visible_ports(policy.peg)
            .map_err(Error::Peg)?;
    }
    resources
        .validate_scan_ownership(policy.graphics)
        .map_err(Error::Peg)?;
    dmi.report_version();
    let dmi_speed = if dmi_link {
        Some(
            dmi.init_link(&sa, pcr, cpu, mode, policy.dmi, msrs, delay)
                .map_err(Error::Dmi)?,
        )
    } else {
        None
    };
    dmi.configure_max_payload(&sa, cpu, mode, policy.dmi)
        .map_err(Error::Dmi)?;
    let peg = if dmi_link {
        Some(
            resources
                .init(dmi, cpu, mode, policy.dmi, policy.peg, msrs, delay)
                .map_err(Error::Peg)?,
        )
    } else {
        None
    };
    dmi.configure_vcs(&sa, pcr, cpu, mode, policy.dmi, delay)
        .map_err(Error::Dmi)?;
    let peg_primary = resources
        .scan_vga(dmi, cpu, mode, policy.dmi, policy.graphics, scan)
        .map_err(Error::Peg)?;
    let graphics = resources
        .graphics(
            dmi,
            cpu,
            mode,
            policy.dmi,
            policy.graphics,
            peg_primary.is_some(),
        )
        .map_err(Error::Peg)?;
    let bclk = match cse.bclk(
        cse::BclkPolicy {
            ult_or_ulx: cpu.package == Package::UltUlx,
            oc_watchdog_control: policy.oc_watchdog_control,
        },
        delay,
    ) {
        Ok(reply) if reply.warm_reset_requested => return Ok(Outcome::WarmResetRequested(reply)),
        Ok(reply) => Bclk::Reply(reply),
        Err(cse::Error::BclkSkipped(reason)) => Bclk::Skipped(reason),
        Err(error) => return Err(Error::Cse(error)),
    };
    resources
        .mch
        .hdaudio_routing
        .modify(HdaAudioRouting::DEVICE_NUMBER.val(0x1f) + HdaAudioRouting::ENABLE::SET);
    resources
        .mch
        .sa_pre_mem_control
        .modify(SaPreMemControl::BIT22::SET);
    Ok(Outcome::Complete(Completed {
        dmi_speed,
        peg,
        peg_primary,
        graphics,
        bclk,
    }))
}
