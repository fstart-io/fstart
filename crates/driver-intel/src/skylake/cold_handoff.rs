// SPDX-License-Identifier: MIT OR Apache-2.0
//! Concrete cold controller -> CSE DID -> caller-owned E820 publication.
//! Source order/actions: romstage_native.c:1064–1125, :202–292.

use super::{
    cold_controller::{self, ControllerError, ControllerReport},
    cold_training::{ColdPolicy, ColdWorkspace},
    cse::{self, Cse, DramAction, DramInitStatus, UmaRequest, ValidatedDram},
    memory_check::MemoryCheckWorkspace,
    memory_map::{self, Plan, ProgrammedMap},
    registers::MchRegisters,
    smram::Smram,
    training_state::CalibrationState,
};
use crate::{
    heci::{Csr, Fifo},
    sunrise_point::pmc::PmcRegisters,
};
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_core::services::{
    ServiceError,
    memory_detect::{E820Entry, E820Kind, E820State, build_pc_compatible_e820},
};
use fstart_pci::EcamDevice;
use tock_registers::interfaces::{Readable, Writeable};

const MIB: u64 = 1 << 20;
const FOUR_GIB: u64 = 1 << 32;

/// Actual caller/platform-owned physical range, not a policy inferred here.
/// Non-Reserved kinds must be entirely in otherwise usable DRAM. Overlapping
/// caller ranges are rejected; coalesce same-role ranges before calling.
#[derive(Debug, Clone, Copy)]
pub struct Reservation {
    pub base: u64,
    pub size: u64,
    pub kind: E820Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    Map(memory_map::Error),
    Geometry,
    Uma,
    Inventory,
    HeciOverlap,
    Reservation(usize),
    Table(ServiceError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Preflight(ValidationError),
    Controller(ControllerError),
    Cse(cse::Error),
    BeforeDid(ValidationError),
    /// DID was delivered; retain its actual action even if a subsequent
    /// identity read fails. Stop, never retry the transaction or publish RAM.
    AfterDid {
        action: DramAction,
        source: ValidationError,
    },
}

/// Genuine controller/SMRAM observations, not a MemoryInit/ReadyMemory token.
/// Compatible SMRAM enable alone is NOT installed or protected SMM.
pub struct Report<'a> {
    pub controller: ControllerReport,
    pub smram: Smram<'a>,
    pub action: DramAction,
}

pub enum Outcome<'a> {
    /// Only this variant publishes count/total in the caller's E820State.
    Complete(Report<'a>),
    /// STOP: table remains empty. Caller owns the reset decision; no reset,
    /// PMC reset-control update, DISB, stage transition or power I/O is executed.
    ResetRequired(Report<'a>),
    /// STOP: validated response but no authorization to continue (actions0/3/4/5).
    /// Retains the actual action; no usable table or reset action is fabricated.
    Stopped(Report<'a>),
}

fn reset_required(action: DramAction) -> bool {
    matches!(
        action,
        DramAction::NonPowerCycleReset | DramAction::PowerCycleReset | DramAction::GlobalReset
    )
}

pub(crate) fn low_top(p: &Plan, uma: UmaRequest) -> Result<u64, ValidationError> {
    let me_size = u64::from(uma.mib()) * MIB;
    let available = p.tom.checked_sub(me_size).ok_or(ValidationError::Uma)?;
    if p.me_size_mib != u32::from(uma.mib())
        || p.me_physical_base != if me_size == 0 { None } else { Some(available) }
    {
        return Err(ValidationError::Uma);
    }
    let prm = p.prmrr;
    let size = u64::from(prm.size_mib()) * MIB;
    let low = if size == 0 { p.tseg } else { prm.base() };
    if low < MIB
        || low > p.tseg
        || p.tseg >= p.bgsm
        || p.bgsm > p.bdsm
        || p.bdsm > p.tolud
        || p.tolud >= FOUR_GIB
        || p.tolud > available
        || p.touud < FOUR_GIB
        || [
            p.tom,
            low,
            p.tseg,
            p.bgsm,
            p.bdsm,
            p.tolud,
            p.touud,
            p.remap_base,
            p.remap_limit,
        ]
        .iter()
        .any(|v| v & (MIB - 1) != 0)
        || !(p.bgsm - p.tseg).is_power_of_two()
        || p.tseg & (p.bgsm - p.tseg - 1) != 0
        || size != 0
            && (!size.is_power_of_two()
                || low & (size - 1) != 0
                || low.checked_add(size).is_none_or(|end| end > p.tseg))
    {
        return Err(ValidationError::Geometry);
    }
    if available > p.tolud {
        let base = FOUR_GIB.max(available);
        let top = base
            .checked_add(FOUR_GIB.min(available) - p.tolud)
            .ok_or(ValidationError::Geometry)?;
        if p.remap_base != base || p.touud != top || p.remap_limit.checked_add(MIB) != Some(top) {
            return Err(ValidationError::Geometry);
        }
    } else if p.remap_base <= p.remap_limit || p.touud != FOUR_GIB {
        return Err(ValidationError::Geometry);
    }
    Ok(low)
}

fn inventory(map: &ProgrammedMap, policy: &ColdPolicy<'_>) -> Result<(), ValidationError> {
    let mut total = 0u64;
    for d in policy.memory().dimms.iter().flatten().flatten() {
        total = total
            .checked_add(u64::from(d.size_mib) * MIB)
            .ok_or(ValidationError::Inventory)?;
    }
    if total != map.plan().tom {
        return Err(ValidationError::Inventory);
    }
    Ok(())
}

/// Check an E820 map against the programmed controller geometry.
pub(crate) fn validate_table_geometry(
    entries: &[E820Entry],
    p: &Plan,
    uma: UmaRequest,
) -> Result<u64, ValidationError> {
    let low = low_top(p, uma)?;
    let mut total = 0u64;
    let mut previous_end = 0;
    for entry in entries {
        let end = entry
            .addr
            .checked_add(entry.size)
            .ok_or(ValidationError::Geometry)?;
        let kind = entry.kind;
        let ordinary_dram = (entry.addr >= 0x1000 && end <= 0x9f000)
            || (entry.addr >= MIB && end <= low)
            || (entry.addr >= FOUR_GIB && end <= p.touud);
        // Gap-free, except for the PCI hole between TOLUD and 4 GiB.
        let pci_hole = previous_end == p.tolud && entry.addr == FOUR_GIB;
        if entry.size == 0
            || (entry.addr != previous_end && !pci_hole)
            || end > p.touud
            || !(1..=5).contains(&kind)
            || (kind != E820Kind::Reserved as u32 && !ordinary_dram)
        {
            return Err(ValidationError::Geometry);
        }
        previous_end = end;
        if kind == E820Kind::Ram as u32 {
            total = total
                .checked_add(entry.size)
                .ok_or(ValidationError::Geometry)?;
        }
    }
    // Without high DRAM the table ends at TOLUD.
    let top = if p.touud > FOUR_GIB { p.touud } else { p.tolud };
    if previous_end != top || total == 0 {
        return Err(ValidationError::Geometry);
    }
    Ok(total)
}

fn validate_reservations(
    p: &Plan,
    uma: UmaRequest,
    reservations: &[Reservation],
) -> Result<u64, ValidationError> {
    let low = low_top(p, uma)?;
    for (i, r) in reservations.iter().enumerate() {
        let end = r
            .base
            .checked_add(r.size)
            .filter(|end| *end <= p.touud)
            .ok_or(ValidationError::Reservation(i))?;
        if r.size == 0
            || r.kind == E820Kind::Ram
            || r.kind != E820Kind::Reserved
                && !((r.base >= 0x1000 && end <= 0x9f000)
                    || (r.base >= MIB && end <= low)
                    || (r.base >= FOUR_GIB && end <= p.touud))
        {
            return Err(ValidationError::Reservation(i));
        }
        for other in &reservations[..i] {
            // Earlier spans have already passed checked-add validation.
            if r.base < other.base + other.size && other.base < end {
                return Err(ValidationError::Reservation(i));
            }
        }
    }
    Ok(low)
}

// Count/total are temporarily private construction state under exclusive &mut.
// Always invalidate before returning, including any capacity/validation failure.
// Raw entries are unspecified on failure; entries()/count()/total_ram() are empty.
// Keep the table scratch frame out of the training call chain in CAR.
#[inline(never)]
fn stage_table(
    table: &mut E820State,
    p: &Plan,
    uma: UmaRequest,
    reservations: &[Reservation],
) -> Result<(usize, u64), ValidationError> {
    table.set_detected(0, 0).map_err(ValidationError::Table)?;
    let result = (|| {
        let low = validate_reservations(p, uma, reservations)?;
        let entries = table.entries_mut();
        let count = build_pc_compatible_e820(entries, low as u32, p.touud, p.tolud as u32)
            .map_err(ValidationError::Table)?;
        // TOLUD..4GiB stays a gap, like native coreboot: it holds the PCI
        // host bridge window that ACPI _CRS describes. Linux clips E820
        // reservations out of that window, so reserving it leaves PCI BARs
        // nowhere to go. Fixed chipset decodes are ACPI motherboard resources.
        table
            .set_detected(count, 0)
            .map_err(ValidationError::Table)?;
        for r in reservations {
            table
                .reserve_range_as(r.base, r.size, r.kind)
                .map_err(ValidationError::Table)?;
        }
        let total = validate_table_geometry(table.entries(), p, uma)?;
        Ok((table.count(), total))
    })();
    table.set_detected(0, 0).map_err(ValidationError::Table)?;
    result
}

/// The E820 map training published for the programmed controller `p`,
/// rebuilt by the stages after it.
pub(crate) fn build_table(
    table: &mut E820State,
    p: &Plan,
    uma: UmaRequest,
    reservations: &[Reservation],
) -> Result<(), ValidationError> {
    let (count, total) = stage_table(table, p, uma, reservations)?;
    table
        .set_detected(count, total)
        .map_err(ValidationError::Table)
}

fn commit_table(
    table: &mut E820State,
    count: usize,
    total: u64,
    action: DramAction,
) -> Result<bool, Error> {
    if action != DramAction::ContinuePost {
        table.set_detected(0, 0).map_err(|e| Error::AfterDid {
            action,
            source: ValidationError::Table(e),
        })?;
        return Ok(false);
    }
    table
        .set_detected(count, total)
        .map_err(|e| Error::AfterDid {
            action,
            source: ValidationError::Table(e),
        })?;
    Ok(true)
}

// No controller callback/report argument. This private message seam is tested
// with disclosed synthetic maps + the real existing CSE transport, not training.
// Safety: genuine controller completion in production; synthetic tests certify
// only protocol/geometry behavior and may not use this to publish usable RAM.
unsafe fn did<H, M, W, R>(
    cse: &mut Cse<H, M, W, R>,
    sa: &EcamDevice,
    map: &ProgrammedMap,
    uma: UmaRequest,
    status: DramInitStatus,
    delay: &mut impl DelayNs,
) -> Result<DramAction, Error>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    low_top(map.plan(), uma).map_err(Error::BeforeDid)?;
    map.validate_live(sa)
        .map_err(|e| Error::BeforeDid(ValidationError::Map(e)))?;
    // SAFETY: reached in production only after the actual controller returned;
    // actual returned map and UMA identity were rechecked immediately above.
    let dram = unsafe { ValidatedDram::new(uma, map.plan().me_physical_base.unwrap_or(0), status) }
        .map_err(Error::Cse)?;
    cse.initialize(delay).map_err(Error::Cse)?;
    let action = cse.dram_init_done(sa, dram, delay).map_err(Error::Cse)?;
    map.validate_live(sa).map_err(|e| Error::AfterDid {
        action,
        source: ValidationError::Map(e),
    })?;
    Ok(action)
}

/// Run actual cold training/finalization, then native DID, then commit E820.
/// Only an actual validated ContinuePost(7) authorizes publication. Reset actions
/// 1/2/6 and ignored/no-ACK actions0/3/4/5 stop without publishing the table.
///
/// # Safety
/// All cold_controller::train_and_finalize_cold obligations apply unchanged,
/// including actual platform-approved RDRAND, physical0 CPU test ownership,
/// real cold eight-lane standard DDR4/SA-GV-off/ECC-off/TXT-inactive hardware,
/// destructive training/workspace exclusion, no active DPR, and actual CPU/
/// MCH/PMC/SA/map endpoints. `uma` must be the actual wait result used to create
/// `map`, retained outside retries. CSE must own the actual HECI1 and stable BAR;
/// its existing endpoint/transport safety contracts apply. No competing AP,
/// DMA, interrupts or register/table writers may run through this whole call.
///
/// `reservations` must enumerate ALL additional represented physical platform
/// and boot-owned memory, including live code/stack/table/workspaces/firmware
/// allocations that will survive into the consumer. Their physical identity,
/// completeness, lifetime and ACPI/NVS semantics are the platform's obligation,
/// not inferred from Rust pointers. No PTT/CBMEM/SMM policy is invented here.
/// This does not establish VT-d translation, SMRR, SMM installation or CAR exit.
///
/// `table` is caller-owned construction storage, not a global cache. Its old
/// contents are invalidated on entry. It stays count=total=0 on EVERY error or
/// ResetRequired/Stopped; raw storage may be partially changed and MUST NOT be handed
/// off via entries_mut(). Only Complete publishes entries()/total_ram(). The
/// reservation geometry is checked before training. The table is built under
/// exclusive borrow after controller completion, before transmitting DID.
/// No second E820State is allocated, but core reserve_range_as uses its existing
/// local scratch array; see documentation, no CAR-fit claim is made.
///
/// After controller execution begins, ANY error or non-Complete outcome is a fatal
/// potentially partial hardware state (locks/SMRAM/DID may already have changed).
/// Caller retains ownership of hardware and mutated calibration/workspaces; stop,
/// never replay/retrain/roll back or publish usable RAM. Error discards report/
/// SMRAM binding, not hardware changes. ResetRequired carries genuine outputs
/// and the actual requested action without executing it; Stopped retains the
/// non-authorizing action. DISB remains external.
#[allow(clippy::too_many_arguments)]
pub unsafe fn train_and_handoff_cold<'a, H, M, W, R>(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    sa: &'a EcamDevice,
    map: &'a ProgrammedMap,
    cpu: &impl MsrAccess,
    training_policy: &ColdPolicy<'_>,
    policy: cold_controller::Policy,
    state: &mut CalibrationState,
    training_work: &mut ColdWorkspace,
    check_work: &mut MemoryCheckWorkspace,
    mapped_base: usize,
    mapped_length: usize,
    cse: &mut Cse<H, M, W, R>,
    uma: UmaRequest,
    status: DramInitStatus,
    reservations: &[Reservation],
    table: &mut E820State,
    cache: cold_controller::Cache<'_>,
    delay: &mut impl DelayNs,
) -> Result<Outcome<'a>, Error>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    table
        .set_detected(0, 0)
        .map_err(|e| Error::Preflight(ValidationError::Table(e)))?;
    fstart_log::debug!("DRAM handoff: live map and DIMM inventory");
    map.validate_live(sa)
        .map_err(|e| Error::Preflight(ValidationError::Map(e)))?;
    inventory(map, training_policy).map_err(Error::Preflight)?;
    fstart_log::debug!("DRAM handoff: CSE DID preflight");
    let heci = cse.preflight_did(uma).map_err(Error::Cse)? as u64;
    if heci < map.plan().tolud || heci.checked_add(0x1000).is_none_or(|end| end > FOUR_GIB) {
        return Err(Error::Preflight(ValidationError::HeciOverlap));
    }
    validate_reservations(map.plan(), uma, reservations).map_err(Error::Preflight)?;
    let (controller, smram) = unsafe {
        cold_controller::train_and_finalize_cold(
            registers,
            pmc,
            sa,
            map,
            cpu,
            training_policy,
            policy,
            state,
            training_work,
            check_work,
            mapped_base,
            mapped_length,
            cache,
            delay,
        )
    }
    .map_err(Error::Controller)?;
    // As in the native reference, keep handoff bookkeeping out of the
    // training entry path. Finish the private table before sending DID.
    let (count, total) =
        stage_table(table, map.plan(), uma, reservations).map_err(Error::BeforeDid)?;
    fstart_log::debug!("DRAM: controller checks passed; sending CSE DRAM Init Done");
    // SAFETY: actual controller just returned; no caller report/bool bypass.
    let action = unsafe { did(cse, sa, map, uma, status, delay) }?;
    fstart_log::debug!("DRAM: CSE action {:?}", fstart_log::DebugValue(&action));
    let report = Report {
        controller,
        smram,
        action,
    };
    if commit_table(table, count, total, action)? {
        Ok(Outcome::Complete(report))
    } else if reset_required(action) {
        Ok(Outcome::ResetRequired(report))
    } else {
        Ok(Outcome::Stopped(report))
    }
}

#[cfg(test)]
#[path = "cold_handoff_tests.rs"]
mod tests;
