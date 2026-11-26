// SPDX-License-Identifier: MIT OR Apache-2.0
//! Concrete bounded cold training or cached replay + native controller
//! finalization. Not MemoryInit, usable RAM, or a stage/SMM handoff.

use super::{
    Error,
    activation::{ActivationError, ActivationPolicy, PowerDown},
    cold_training::{self, ColdError, ColdPolicy, ColdTrainingReport, ColdWorkspace},
    cpu_memory_test,
    energy_perf_gain::{self, Outcome, Profile},
    memory_check::MemoryCheckWorkspace,
    memory_map::ProgrammedMap,
    registers::MchRegisters,
    reut::{Engine, rank_mask},
    smram::{self, Smram},
    training_cache::Record,
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::{MemoryBootMode, PmcRegisters};
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::{self, cpu::msr::MsrAccess};
use fstart_pci::EcamDevice;

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Actual board/source policy; never inferred from package or fan policy.
    pub power_down: PowerDown,
    /// Native MEMTST is OFF. Explicit opt-in runs the existing full-rank test.
    pub memory_test: bool,
    /// Native clean_memory=false/ecc=false scrub is a real no-op.
    pub clean_memory: bool,
    /// Native EPG is OFF. Some explicitly selects the existing source model.
    pub energy_perf_gain: Option<Profile>,
    /// TXT-active flows (GETSEC/ACM/reset) are unsupported, never silently skipped.
    pub txt_active: bool,
}
impl Policy {
    pub const fn native_defaults(power_down: PowerDown) -> Self {
        Self {
            power_down,
            memory_test: false,
            clean_memory: false,
            energy_perf_gain: None,
            txt_active: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    PostTraining,
    AliasCheck,
    MemoryTest,
    Scrub,
    EnergyPerfGain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerError {
    Preflight(Error),
    CpuBounds(cpu_memory_test::Error),
    Training(ColdError),
    Tail { phase: Phase, source: Error },
    Activation(ActivationError),
    McDone(ActivationError),
    CpuTest(cpu_memory_test::Error),
    Smram(smram::Error),
}

/// Observations only: skipped full tests/scrub must never be described as ran.
/// final_* remain unset; compatible SMRAM enable is NOT installed/protected SMM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerReport {
    /// `None` when the cached training was replayed instead.
    pub training: Option<ColdTrainingReport>,
    /// Length of the training record written to the caller's capture buffer.
    pub captured: Option<usize>,
    pub alias_check_ran: bool,
    pub full_memory_test_ran: bool,
    pub memory_zeroed: bool,
    pub energy_perf_gain: Outcome,
    pub cpu_first_4k_test_ran: bool,
}

/// Training cache inputs. `record` was parsed for `identity`; `capture`
/// receives the record this boot leaves behind (empty: capture nothing).
pub struct Cache<'c> {
    pub identity: [u8; 32],
    pub record: Option<&'c Record>,
    pub capture: &'c mut [u8],
}

/// Native post_training.c mc_activate(): low TSC bits XOR the channel tag.
/// These are DRAM scrambler seeds, NOT cryptographic keys or qualified entropy.
/// Zero is a valid result in the reference and must not turn into a boot gate.
fn scrambler_key(tsc: u64, channel: u8) -> u16 {
    (tsc as u16) ^ (u16::from(channel) << 8)
}
fn sample_keys(ranks: [u8; 2]) -> [u16; 2] {
    let mut keys = [0; 2];
    for ch in 0..2 {
        if ranks[ch] != 0 {
            keys[ch] = scrambler_key(x86::rdtsc(), ch as u8);
        }
    }
    keys
}

fn preflight(
    mode: MemoryBootMode,
    policy: Policy,
    mapped_base: usize,
    mapped_length: usize,
) -> Result<(), ControllerError> {
    cpu_memory_test::validate_bounds(mapped_base, mapped_length, mode)
        .map_err(ControllerError::CpuBounds)?;
    if policy.txt_active {
        return Err(ControllerError::Preflight(Error::InvalidPolicy));
    }
    Ok(())
}

/// Execute the actual bounded training owner (or cached restore on fast/S3),
/// then every supported native tail step in order, then CPU first-4KiB test
/// (non-destructive on S3) and compatible SMRAM enable.
/// Never invokes a reset, publishes a cache, or sets final_* training fields.
///
/// # Safety
/// All train_cold, activation, MC-done, CPU basic_test and Smram::enable_cold
/// obligations apply. Caller owns the actual unlocked SA/MCHBAR/PMC/MSRs,
/// PCODE, DRAM and caller-reserved workspaces exclusively (UC register mappings,
/// no AP/DMA/interrupt/config competitors). Early-SA/reset decisions are already
/// resolved. Retain the actual system ProgrammedMap and CSE UMA identity OUTSIDE
/// retries; this function never allocates, reprograms or relocks them per attempt.
/// `training_policy` must describe that same real inventory, CPU and map.
/// Only DDR4/eight lanes/SA-GV-off/ECC-off/TXT-inactive cold or S3 boot is supported.
/// Scrambler seeds follow the reference's per-channel TSC sampling; no claim
/// of cryptographic entropy is made.
///
/// mapped_base/length are NOT mapping evidence: the first 4096 bytes must truly
/// map physical 0..4096 to initialized DRAM, exclusively owned, with no Rust
/// references even at 0. Caller establishes effective cache behavior (volatile
/// tests alone do not prove DRAM rather than cache). Keep code, stack, CAR and
/// both workspaces outside this page and (on cold boot) destructive REUT ranges.
///
/// ANY error after training starts is fatal potentially partial hardware. Stop;
/// do not replay, roll back, retrain or perform a success handoff. Training Required
/// is handled ONLY inside train_cold; no failed/Required attempt reaches normal
/// mode or locks. Returned Smram is only compatible-SMRAM control, NOT SMRR/MP/
/// WB/save-state readiness (generic SMM's optional legacy SMRR policy is not it).
/// CSE DID/reset actions, E820/reservations, CPU/SMM install and CAR/stages/image
/// remain separate outer contracts. No MemoryInit/ReadyMemory certificate.
#[allow(clippy::too_many_arguments)]
pub unsafe fn train_and_finalize_cold<'a>(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    sa: &'a EcamDevice,
    map: &'a ProgrammedMap,
    cpu: &impl MsrAccess,
    mode: MemoryBootMode,
    training_policy: &ColdPolicy<'_>,
    policy: Policy,
    state: &mut CalibrationState,
    training_work: &mut ColdWorkspace,
    check_work: &mut MemoryCheckWorkspace,
    mapped_base: usize,
    mapped_length: usize,
    cache: Cache<'_>,
    delay: &mut impl DelayNs,
) -> Result<(ControllerReport, Smram<'a>), ControllerError> {
    preflight(mode, policy, mapped_base, mapped_length)?;
    let engine = Engine {
        registers,
        dimms: &training_policy.memory().dimms,
    };
    if let Some(record) = cache.record {
        fstart_log::debug!("DRAM handoff: replay cached DDR4 training");
        unsafe {
            cold_training::restore_fast(registers, pmc, training_policy, mode, state, record, delay)
        }
        .map_err(ControllerError::Training)?;
        engine
            .meminit_normal_mode_fast(
                mode,
                training_policy.cpu().package,
                policy.power_down,
                delay,
            )
            .map_err(ControllerError::Activation)?;
        // On S3 resume never write back to flash; on fast cold boot the journal
        // skips a byte-identical record so there is no flash write either.
        let captured = if mode == MemoryBootMode::S3 {
            None
        } else {
            cache
                .capture
                .get_mut(..core::mem::size_of_val(record))
                .map(|out| {
                    out.copy_from_slice(zerocopy::IntoBytes::as_bytes(record));
                    out.len()
                })
        };
        return unsafe {
            finish(
                &engine,
                sa,
                map,
                cpu,
                mode,
                training_policy,
                policy,
                None,
                captured,
                state,
                check_work,
                mapped_base,
                mapped_length,
                delay,
            )
        };
    }
    if mode != MemoryBootMode::Cold {
        // S3 resume MUST replay cached training; never fall through to
        // destructive cold training over suspended OS memory.
        return Err(ControllerError::Preflight(Error::RetainedMemoryUnsupported));
    }
    // Native cold entry does not probe MC_LOCK or DDR_PTM before training.
    // The caller already rejected retained boot. Check the locks in the
    // post-training activation/finalization paths that use those registers.
    fstart_log::debug!("DRAM handoff: enter DDR4 trainer");
    let training = unsafe {
        cold_training::train_cold(registers, pmc, training_policy, state, training_work, delay)
    }
    .map_err(ControllerError::Training)?;
    let ranks = [
        rank_mask(engine.dimms, 0).map_err(ControllerError::Preflight)?,
        rank_mask(engine.dimms, 1).map_err(ControllerError::Preflight)?,
    ];
    let keys = sample_keys(ranks);
    // LOCAL control-flow fact: train_cold returned only after real TAT and the
    // accepted finalmeter/TXVref/MR-scratch. No caller-supplied training_done.
    unsafe {
        finish_cold(
            &engine,
            sa,
            map,
            cpu,
            training_policy,
            policy,
            training,
            keys,
            state,
            check_work,
            mapped_base,
            mapped_length,
            Some(cache),
            delay,
        )
    }
}

// Private tail-only seam permits disclosed synthetic calibration/keys in tests.
// There is no production entry bypassing genuine train_cold or TSC sampling.
#[allow(clippy::too_many_arguments)]
unsafe fn finish_cold<'a>(
    engine: &Engine<'_>,
    sa: &'a EcamDevice,
    map: &'a ProgrammedMap,
    cpu: &impl MsrAccess,
    training_policy: &ColdPolicy<'_>,
    policy: Policy,
    training: ColdTrainingReport,
    keys: [u16; 2],
    state: &mut CalibrationState,
    work: &mut MemoryCheckWorkspace,
    mapped_base: usize,
    mapped_length: usize,
    cache: Option<Cache<'_>>,
    delay: &mut impl DelayNs,
) -> Result<(ControllerReport, Smram<'a>), ControllerError> {
    let mode = MemoryBootMode::Cold;
    let tail_error = |phase, source| ControllerError::Tail { phase, source };
    engine
        .meminit_post_training(mode, training_policy.cpu().package, policy.power_down)
        .map_err(|e| tail_error(Phase::PostTraining, e))?;
    unsafe {
        engine.meminit_mc_activate(
            state,
            mode,
            training_policy.timings().clock,
            ActivationPolicy {
                package: training_policy.cpu().package,
                power_down: policy.power_down,
                thermal: training.thermal, // actual mutated OLTM output
                ecc: false,
                sa_gv: false,
                turnaround_trained: true,
                scrambling_keys: keys,
            },
            delay,
        )
    }
    .map_err(ControllerError::Activation)?;
    // Native meminit_save_training(): after mc_activate, before the alias
    // check and memory test touch REUT state.
    let captured = cache
        .and_then(|cache| Record::capture(cache.capture, engine.registers, state, cache.identity));
    unsafe {
        finish(
            engine,
            sa,
            map,
            cpu,
            mode,
            training_policy,
            policy,
            Some(training),
            captured,
            state,
            work,
            mapped_base,
            mapped_length,
            delay,
        )
    }
}

/// The tail shared by cold training and cached replay (fast cold and S3):
/// optional destructive checks (cold only), EPG, MC done, CPU first-4-KiB test
/// (non-destructive on S3) and compatible SMRAM.
#[allow(clippy::too_many_arguments)]
unsafe fn finish<'a>(
    engine: &Engine<'_>,
    sa: &'a EcamDevice,
    map: &'a ProgrammedMap,
    cpu: &impl MsrAccess,
    mode: MemoryBootMode,
    training_policy: &ColdPolicy<'_>,
    policy: Policy,
    training: Option<ColdTrainingReport>,
    captured: Option<usize>,
    state: &mut CalibrationState,
    work: &mut MemoryCheckWorkspace,
    mapped_base: usize,
    mapped_length: usize,
    delay: &mut impl DelayNs,
) -> Result<(ControllerReport, Smram<'a>), ControllerError> {
    let tail_error = |phase, source| ControllerError::Tail { phase, source };
    let destructive = mode == MemoryBootMode::Cold;
    let alias = destructive && training_policy.gates().training_enables & (1 << 28) != 0;
    if alias {
        engine
            .meminit_alias_check(state, mode, false, work, delay)
            .map_err(|e| tail_error(Phase::AliasCheck, e))?;
    }
    let full_memory_test_ran = destructive && policy.memory_test;
    if full_memory_test_ran {
        engine
            .meminit_memory_test(state, mode, false, work, delay)
            .map_err(|e| tail_error(Phase::MemoryTest, e))?;
    }
    let memory_zeroed = destructive && policy.clean_memory;
    if destructive {
        engine
            .memory_scrub(state, mode, false, policy.clean_memory, work, delay)
            .map_err(|e| tail_error(Phase::Scrub, e))?;
    }
    let energy = energy_perf_gain::meminit_energy_perf_gain(
        engine.registers,
        engine.dimms,
        training_policy.phy(),
        energy_perf_gain::Policy {
            enabled: policy.energy_perf_gain.is_some(),
            profile: policy.energy_perf_gain,
            sa_gv: false,
        },
        delay,
    )
    .map_err(|e| tail_error(Phase::EnergyPerfGain, e))?;
    // SA-GV disabled; TXT inactive was rejected before training if requested.
    unsafe { engine.meminit_mc_done(mode, false, sa, map, cpu, delay) }
        .map_err(ControllerError::McDone)?;
    if destructive {
        unsafe { cpu_memory_test::basic_test(mapped_base, mapped_length, mode) }
            .map_err(ControllerError::CpuTest)?;
    }
    let smram = unsafe { Smram::enable_cold(sa, map, mode) }.map_err(ControllerError::Smram)?;
    Ok((
        ControllerReport {
            training,
            captured,
            alias_check_ran: alias,
            full_memory_test_ran,
            memory_zeroed,
            energy_perf_gain: energy,
            cpu_first_4k_test_ran: destructive,
        },
        smram,
    ))
}
