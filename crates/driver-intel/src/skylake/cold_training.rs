// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded native training.c DDR4 cold attempt owner (not MemoryInit).
//!
//! Only standard-profile, eight-data-lane, SA-GV-off operation is represented.
//! Policy is resolved once from retained SPD/fuses/CPU/electrical inputs. All
//! enabled phases below are concrete algorithms, never caller phase callbacks.

use super::{
    Error,
    additional_steps::OltmPolicy,
    address_decode::AddressDecode,
    capabilities::{Capabilities, MemoryConfiguration},
    command_si::CommandSiWorkspace,
    command_training::{CommandContext, CommandPolicy},
    cpu::{Cpu, Package},
    dq_si::{DimmOdtWorkspace, DqSiPolicy, DqSiWorkspace},
    frequency, jedec,
    mc_timing::TimingRegisters,
    opt_param::OptWorkspace,
    overrides::{GdxcRegisters, PostSpdOverrides},
    phy::{self, PhyPolicy},
    post_services::{PostServicesPolicy, PostServicesWorkspace, RetrainOutcome},
    power_model::{self, LimitKind, MarginKind, PowerModel, SystemPower, TrainingMode},
    read_centering::ReadCenteringWorkspace,
    registers::MchRegisters,
    reut::{Engine, rank_mask},
    spd::{Dimm, Dimms},
    timings::{self, Timings},
    training_state::CalibrationState,
    turnaround_training::LatencyWorkspace,
    write_centering::WriteCenteringWorkspace,
};
use crate::sunrise_point::pmc::{MemoryBootMode, PmcRegisters};
use embedded_hal::delay::DelayNs;

/// Native romstage_native.c default masks. RMT is BIT26, not relative BER.
/// ECT, RMT, MEMTST, WRDSUDT are OFF; WRDSEQT is DT/Halo-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeGates {
    pub training_enables: u32,
    pub training_enables2: u32,
}
impl NativeGates {
    pub const fn for_package(package: Package) -> Self {
        Self {
            training_enables: 0x73ff_fffe
                & if matches!(package, Package::UltUlx) {
                    !(1 << 13)
                } else {
                    u32::MAX
                },
            training_enables2: 0x1f,
        }
    }
}

/// Board/stage inputs, not guessed M900 defaults. Pre-SPD overrides and any
/// requested reset must ALREADY be resolved before constructing this policy.
#[derive(Debug, Clone, Copy)]
pub struct ColdPolicyInput {
    pub cpu: Cpu,
    pub capabilities: Capabilities,
    pub overrides: PostSpdOverrides,
    /// Optional platform speed ceiling, expressed as minimum period in fs.
    /// This cannot bypass either actual fuse limits or SPD standard timings.
    pub platform_tck_min_fs: u32,
    pub phy: PhyPolicy,
    pub dram_odt: bool,
    pub ca_vref_config: u8,
    pub dummy_reads: bool,
    pub thermal: OltmPolicy,
}

/// Immutable resolved inputs. No field is an acceptance/cached-RAM token.
/// SPD backing storage and the GDXC mapping must remain live and unchanged.
pub struct ColdPolicy<'a> {
    input: ColdPolicyInput,
    inventory: Dimms<'a>,
    memory: MemoryConfiguration<'a>,
    timings: Timings,
    overrides: PostSpdOverrides,
    gdxc: &'a GdxcRegisters,
}
impl<'a> ColdPolicy<'a> {
    pub fn resolve(
        inventory: &Dimms<'a>,
        gdxc: &'a GdxcRegisters,
        input: ColdPolicyInput,
    ) -> Result<Self, Error> {
        if Cpu::from_signature(input.cpu.signature)? != input.cpu {
            return Err(Error::UnsupportedCpu);
        }
        if input.platform_tck_min_fs == 0
            || !matches!(input.ca_vref_config, 0 | 2)
            || input.thermal.refresh_2x > 2
            || input.phy.vdd_mv != 1200
            || input.overrides.gdxc_enable
        {
            return Err(Error::InvalidPolicy);
        }
        for d in inventory.iter().flatten().flatten() {
            if Dimm::decode(d.raw)? != *d {
                return Err(Error::UnsupportedDimm);
            }
        }
        // Check unavailable fuse words before computing a source clock floor.
        if [
            input.capabilities.capid_a,
            input.capabilities.capid_b,
            input.capabilities.capid_c,
        ]
        .contains(&u32::MAX)
        {
            return Err(Error::HostBridgeUnavailable);
        }
        let two_dpc = inventory.iter().any(|ch| ch.iter().all(Option::is_some));
        let floor = timings::fused_tck_min(
            input.cpu,
            input.capabilities.capid_a,
            input.capabilities.capid_c,
            two_dpc,
        )
        .max(input.platform_tck_min_fs);
        let timings = timings::decode(inventory, floor)?;
        let overrides = input.overrides.apply_ddr4(gdxc)?;
        let memory =
            input
                .capabilities
                .apply(inventory, &timings, input.cpu, overrides.ecc_support)?;
        if memory.ecc {
            return Err(Error::EccRequired);
        }
        phy::rx_vref_for_clock(&memory.dimms, timings.clock, &input.phy, None)?;
        // Plan representability before the attempt can mutate working state.
        TimingRegisters::for_dimms(
            &timings,
            &memory.dimms,
            input.cpu,
            input.dram_odt,
            [[0; 4]; 2],
        )?;
        AddressDecode::for_dimms(
            &memory.dimms,
            overrides.rank_interleave,
            overrides.enhanced_interleave,
            false,
        )?;
        jedec::ModeRegisters::for_dimms(
            &memory.dimms,
            &timings,
            input.cpu,
            input.phy.board,
            input.phy.vtt_odt,
        )?;
        Ok(Self {
            input,
            inventory: *inventory,
            memory,
            timings,
            overrides,
            gdxc,
        })
    }
    pub(crate) fn cpu(&self) -> Cpu {
        self.input.cpu
    }
    pub(crate) fn phy(&self) -> &PhyPolicy {
        &self.input.phy
    }
    pub fn memory(&self) -> &MemoryConfiguration<'a> {
        &self.memory
    }
    pub fn timings(&self) -> &Timings {
        &self.timings
    }
    pub fn gates(&self) -> NativeGates {
        NativeGates::for_package(self.input.cpu.package)
    }
    pub fn training_mode(&self) -> TrainingMode {
        match self.input.cpu.package {
            Package::DesktopHalo => TrainingMode::Margin,
            Package::UltUlx => TrainingMode::Power,
        }
    }
    fn revalidate(&self) -> Result<(), Error> {
        if self.input.overrides.apply_ddr4(self.gdxc)? != self.overrides
            || self.input.capabilities.apply(
                &self.inventory,
                &self.timings,
                self.input.cpu,
                self.overrides.ecc_support,
            )? != self.memory
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(())
    }
}

/// Caller-owned aggregate; allocate in reserved storage, not a small CAR stack.
/// Each member is the existing algorithm workspace, with no whole-state copy.
pub struct ColdWorkspace {
    pub opt: OptWorkspace,
    pub command: CommandSiWorkspace,
    pub dq: DqSiWorkspace,
    pub dimm: DimmOdtWorkspace,
    pub write: WriteCenteringWorkspace,
    pub read: ReadCenteringWorkspace,
    pub latency: LatencyWorkspace,
    pub post: PostServicesWorkspace,
}

impl ColdWorkspace {
    pub const INITIAL: Self = Self {
        opt: OptWorkspace::ZERO,
        command: CommandSiWorkspace::ZERO,
        dq: DqSiWorkspace::INITIAL,
        dimm: DimmOdtWorkspace::INITIAL,
        write: WriteCenteringWorkspace::INITIAL,
        read: ReadCenteringWorkspace::INITIAL,
        latency: LatencyWorkspace::INITIAL,
        post: PostServicesWorkspace::ZERO,
    };

    /// Initialize reserved storage without a whole-workspace stack temporary.
    /// # Safety
    /// Destination must be aligned, writable, exclusive, nonoverlapping with the
    /// initialization image, and live throughout use. Its old value needs no drop.
    pub unsafe fn initialize_at(destination: *mut Self) {
        const {
            assert!(!core::mem::needs_drop::<Self>());
        }
        static IMAGE: ColdWorkspace = ColdWorkspace::INITIAL;
        unsafe { core::ptr::copy_nonoverlapping(&IMAGE, destination, 1) };
    }
}

impl Default for ColdWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
impl ColdWorkspace {
    fn clear_attempt(&mut self) {
        // SAFETY: exclusive initialized workspace with no drop state; the ROM
        // image is separate. Resetting scratch does not publish calibration.
        unsafe { Self::initialize_at(self) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdPhase {
    Preflight,
    OverridesCapability,
    Clear,
    Pll,
    Phy,
    BaseTiming,
    Mad,
    Odt,
    Rcomp,
    Jedec,
    PreTraining,
    SenseAmp,
    EarlyMpr,
    ReceiveEnable,
    Mpr,
    WriteLeveling,
    WriteLevelingCleanup,
    EarlyWrite,
    WriteTiming,
    WriteVoltage,
    EarlyRead,
    ReadTiming,
    CommandRate,
    CommandTiming,
    CommandVoltage,
    CommandPatternHandoff,
    CommandDriveEq,
    CommandSlew,
    CommandNormalization,
    BaselinePower,
    DimmRon,
    DimmOdt,
    WriteEqDrive,
    ReadOdt,
    WriteSlew,
    ReadEq,
    ReadAmplifier,
    OptimizeComp,
    Oltm,
    Rtl,
    Tat,
    WriteVoltage2d,
    ReadVoltage2d,
    WriteTiming2d,
    ReadTiming2d,
    PostServices,
    FinalPower,
    TxVrefRange,
    MrScratch,
    /// Cached training replay (save_restore.c), instead of every training phase.
    Restore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdError {
    /// Hardware/host may be partially changed. Stop; do not retry or certify RAM.
    Phase {
        phase: ColdPhase,
        source: Error,
    },
    RetrainUnresolved {
        read: bool,
        write: bool,
    },
}

/// Honest bounded output only. No activation, locks, cache validity or usable
/// memory is certified. `final_*` fields in CalibrationState remain unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdTrainingReport {
    pub attempts: u8,
    pub gates: NativeGates,
    pub training_mode: TrainingMode,
    /// RMC actually ran on attempt0 (Power mode only), not merely gated Passed.
    pub first_attempt_rmc_measured: bool,
    /// Source skips RMC on retry; this is explicitly NOT a second measurement.
    pub accepted_attempt_rmc_measured: bool,
    pub thermal: OltmPolicy,
    pub post_optimization_power: SystemPower,
}

fn retry_required(outcome: RetrainOutcome, iteration: u8) -> Result<bool, ColdError> {
    match outcome {
        RetrainOutcome::Passed => Ok(false),
        RetrainOutcome::Required { .. } if iteration == 0 => Ok(true),
        RetrainOutcome::Required { read, write } => {
            Err(ColdError::RetrainUnresolved { read, write })
        }
    }
}

/// Transfer a consumed command-pattern identity only after independently
/// verifying BOTH physical channels and MR engines stopped. CADB disable is
/// not replayable pattern contents; never restore runnable old metadata.
pub(crate) fn release_command_pattern(
    engine: &Engine<'_>,
    state: &mut CalibrationState,
) -> Result<(), Error> {
    super::memory_check::stopped(engine)?;
    state.dq_pat = u8::MAX;
    state.dq_pat_lc = 0;
    Ok(())
}

macro_rules! phase {
    ($name:ident, $call:expr) => {{
        fstart_log::debug!("DDR4 training: {}", stringify!($name));
        $call.map_err(|source| ColdError::Phase {
            phase: ColdPhase::$name,
            source,
        })?
    }};
}

/// Run initial attempt plus at most ONE actual Required retry, source order.
///
/// # Safety
/// Caller owns unlocked cold controller/PMC/PCODE/MR/REUT/CADB exclusively,
/// with correct UC mappings, actual SPD/CPU/fuses and verified electrical policy.
/// Complete early-SA/pre-SPD (including resets), retain the actual system
/// ProgrammedMap/UMA outside this owner, and serialize all mailbox users. No
/// activation/normal-mode tail may have run. Do not re-enter after any error.
/// This routine performs no physical platform reset, mapping or allocation.
pub unsafe fn train_cold(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    policy: &ColdPolicy<'_>,
    state: &mut CalibrationState,
    work: &mut ColdWorkspace,
    delay: &mut impl DelayNs,
) -> Result<ColdTrainingReport, ColdError> {
    let engine = Engine {
        registers,
        dimms: &policy.memory.dimms,
    };
    let mode = MemoryBootMode::Cold;
    let t = &policy.timings;
    let p = &policy.input;
    let training_mode = policy.training_mode();
    let gates = policy.gates();
    let ctx = CommandContext {
        pmc,
        mode,
        policy: CommandPolicy {
            sdram_lanes: 8,
            ddp_shared_clock: p.phy.ddp_shared_clock,
            ca_vref_config: p.ca_vref_config,
        },
    };
    let dq = DqSiPolicy {
        clock: t.clock,
        package: p.cpu.package,
        phy: p.phy,
        mode: training_mode,
    };
    for iteration in 0..=1 {
        fstart_log::debug!("DDR4 training: attempt {}", iteration + 1);
        phase!(OverridesCapability, policy.revalidate());
        // Follow native meminit_training(): establish the DDR clock and wait
        // for initial RCOMP before accessing the memory controller's engines.
        if iteration == 0 {
            phase!(Pll, frequency::initialize(registers, t.clock, delay));
        } else {
            // Do not request/invent another clock or rerun a cold PLL request.
            phase!(Pll, frequency::validate_running(registers, t.clock));
        }
        if iteration != 0 {
            // An already initialized controller must be idle before rerunning
            // its setup. At cold entry, defer MMIO checks until first use.
            phase!(Preflight, super::memory_check::stopped(&engine));
        }
        phase!(
            Preflight,
            if state.dq_pat == super::io_test::DqPattern::Cadb as u8 {
                Err(Error::InvalidReutConfig)
            } else {
                Ok(())
            }
        );
        if iteration == 0 {
            // Do not reset the margin limits accumulated by a retrain.
            power_model::initialize_margin_limits(state);
        }
        phase!(
            Clear,
            state.clear_cold_attempt(mode, iteration, p.dummy_reads)
        );
        work.clear_attempt();
        let mut thermal = p.thermal; // Reset thermal INPUT, retain actual mutated output below.
        phase!(
            Phy,
            phy::program_mc_registers(
                registers,
                delay,
                mode,
                p.cpu,
                &policy.memory,
                t,
                &p.phy,
                &mut state.training
            )
        );
        let base = phase!(
            BaseTiming,
            TimingRegisters::for_dimms(t, engine.dimms, p.cpu, p.dram_odt, [[0; 4]; 2])
        );
        base.program(registers, state); // republishes genuine initial 3N every try
        let mad = phase!(
            Mad,
            AddressDecode::for_dimms(
                engine.dimms,
                policy.overrides.rank_interleave,
                policy.overrides.enhanced_interleave,
                false
            )
        );
        mad.program(registers, delay);
        phase!(
            Odt,
            engine.meminit_power_optimization(state, mode, p.cpu.package, 8)
        );
        phase!(Rcomp, phy::force_rcomp(registers, delay));
        // Native enters JEDEC/CADB only after PHY, timings, MAD and RCOMP.
        // Check that ownership immediately before the first command sequence.
        phase!(Preflight, super::memory_check::stopped(&engine));
        // Full first-init, NOT inner-training reinitialize_cold restoring old MR/PDA.
        phase!(
            Jedec,
            jedec::initialize_cold(
                registers,
                pmc,
                engine.dimms,
                t,
                p.cpu,
                p.phy.board,
                p.phy.vtt_odt,
                p.dram_odt,
                p.phy.vdd_mv,
                p.phy.rcomp_target[1],
                mode,
                state,
                delay
            )
        );
        phase!(
            PreTraining,
            engine.pre_training(mode, p.cpu.package, &state.saved_mr)
        );
        phase!(
            SenseAmp,
            engine.sense_amp_offset(state, mode, t, &p.phy, delay)
        );
        phase!(
            EarlyMpr,
            engine.early_read_mpr_2d(state, mode, p.phy.vtt_odt, true, delay)
        );
        // ECT and EWRDSEQ have no DDR4 branch, regardless of the latter's bit.
        phase!(ReceiveEnable, engine.read_training(state, mode, t, delay));
        phase!(Mpr, engine.read_mpr_training_once(state, mode, delay));
        phase!(
            WriteLeveling,
            engine.write_leveling_feedback(state, mode, t, delay)
        );
        phase!(
            WriteLevelingCleanup,
            engine.write_leveling_cleanup(
                state,
                pmc,
                mode,
                t,
                true,
                p.phy.ddp_shared_clock,
                8,
                delay
            )
        );
        phase!(
            EarlyWrite,
            engine.early_write_timing_2d(state, mode, true, delay)
        );
        phase!(WriteTiming, engine.write_training(state, mode, t, delay));
        phase!(WriteVoltage, engine.write_voltage_1d(state, mode, t, delay));
        phase!(
            EarlyRead,
            engine.early_read_timing_2d(state, mode, t, p.phy.vtt_odt, true, delay)
        );
        phase!(
            ReadTiming,
            engine.read_timing_centering_1d(state, mode, t, delay)
        );
        for ch in 0..2 {
            if phase!(CommandRate, rank_mask(engine.dimms, ch)) != 0 {
                phase!(
                    CommandRate,
                    engine.update_command_rate(state, mode, ch, policy.memory.command_rate, t)
                );
            }
        }
        phase!(CommandTiming, engine.cmd_training(state, &ctx, delay));
        let wrv_limit = phase!(
            CommandVoltage,
            power_model::margin_limit(
                state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                t.clock,
                training_mode
            )
        );
        phase!(
            CommandVoltage,
            engine.cmd_voltage_centering(state, &ctx, t, wrv_limit, delay)
        );
        phase!(
            CommandPatternHandoff,
            release_command_pattern(&engine, state)
        );
        phase!(
            CommandDriveEq,
            engine.meminit_cmd_ds_tx_eq(
                state,
                &ctx,
                t,
                p.cpu.package,
                &p.phy,
                training_mode,
                &mut work.opt,
                &mut work.command,
                delay
            )
        );
        if training_mode != TrainingMode::Power {
            phase!(
                CommandSlew,
                engine.meminit_cmd_slew_rate(
                    state,
                    &ctx,
                    t,
                    p.cpu.package,
                    &p.phy,
                    training_mode,
                    &mut work.opt,
                    &mut work.command,
                    delay
                )
            );
        }
        phase!(
            CommandNormalization,
            engine.cmd_normalization(state, &ctx, delay)
        );
        phase!(
            BaselinePower,
            PowerModel {
                registers,
                dimms: engine.dimms,
                saved_mr: &state.saved_mr,
                clock: t.clock,
                package: p.cpu.package,
                phy: &p.phy
            }
            .power_saving_meter_update(&mut state.odt_power_saving, 8)
        );
        phase!(
            DimmRon,
            engine.meminit_dimm_ron_training(
                state,
                &ctx,
                t.clock,
                p.cpu.package,
                &p.phy,
                training_mode,
                &mut work.opt,
                delay
            )
        );
        phase!(
            DimmOdt,
            engine.meminit_dimm_odt_training(
                state,
                &ctx,
                dq,
                &mut work.opt,
                &mut work.dq,
                &mut work.write,
                &mut work.dimm,
                delay
            )
        );
        if gates.training_enables & (1 << 13) != 0 {
            phase!(
                WriteEqDrive,
                engine.meminit_write_eq_ds_training(
                    state,
                    &ctx,
                    dq,
                    &mut work.opt,
                    &mut work.dq,
                    delay
                )
            );
        }
        // WRDSUDT is default OFF; no permissive gate can replace a mandatory phase.
        phase!(
            ReadOdt,
            engine.meminit_read_odt_training(
                state,
                &ctx,
                dq,
                &mut work.opt,
                &mut work.dq,
                &mut work.write,
                delay
            )
        );
        phase!(
            WriteSlew,
            engine.meminit_write_slew_rate(state, &ctx, dq, &mut work.opt, &mut work.dq, delay)
        );
        phase!(
            ReadEq,
            engine.meminit_read_eq_training(state, &ctx, dq, &mut work.opt, &mut work.dq, delay)
        );
        phase!(
            ReadAmplifier,
            engine.meminit_read_amplifier_power(
                state,
                &ctx,
                dq,
                &mut work.opt,
                &mut work.dq,
                &mut work.write,
                delay
            )
        );
        phase!(
            OptimizeComp,
            engine.meminit_optimize_comp(state, &ctx, delay)
        );
        phase!(Oltm, engine.meminit_force_oltm(mode, &mut thermal));
        phase!(
            Rtl,
            engine.meminit_round_trip_latency(state, &ctx, &mut work.latency, delay)
        );
        let qclk_ps = phase!(Tat, frequency::qclk_ps(registers, t.clock));
        phase!(
            Tat,
            engine.meminit_turnaround_timing(state, &ctx, qclk_ps, &mut work.latency, delay)
        );
        phase!(
            WriteVoltage2d,
            engine.meminit_write_voltage_centering_2d(state, &ctx, t.clock, &mut work.write, delay)
        );
        phase!(
            ReadVoltage2d,
            engine.meminit_centering_2d(state, &ctx, &mut work.read, delay)
        );
        phase!(
            WriteTiming2d,
            engine.meminit_write_timing_centering_2d(state, &ctx, &mut work.write, delay)
        );
        phase!(
            ReadTiming2d,
            engine.meminit_read_timing_centering_2d(
                state,
                &ctx,
                t.clock,
                training_mode,
                &mut work.read,
                delay
            )
        );
        // RCVENC1D is enabled but LPDDR3-only. Literal RMT remains default OFF.
        let outcome = phase!(
            PostServices,
            engine.meminit_post_services(
                state,
                &ctx,
                t.clock,
                PostServicesPolicy {
                    training_enables: gates.training_enables,
                    training_mode
                },
                &mut work.post,
                delay
            )
        );
        if retry_required(outcome, iteration)? {
            continue;
        }
        state.upm_pwr_retrain_flag = 0;
        let power = phase!(
            FinalPower,
            PowerModel {
                registers,
                dimms: engine.dimms,
                saved_mr: &state.saved_mr,
                clock: t.clock,
                package: p.cpu.package,
                phy: &p.phy
            }
            .power_saving_meter_update(&mut state.odt_power_saving, 8)
        );
        phase!(
            TxVrefRange,
            engine.meminit_tx_vref_range(state, &ctx, true, false, delay)
        );
        phase!(MrScratch, engine.meminit_fill_mr_scratch(state, &ctx));
        return Ok(ColdTrainingReport {
            attempts: iteration + 1,
            gates,
            training_mode,
            first_attempt_rmc_measured: training_mode == TrainingMode::Power,
            accepted_attempt_rmc_measured: training_mode == TrainingMode::Power && iteration == 0,
            thermal,
            post_optimization_power: power,
        });
    }
    unreachable!("Required on the last attempt is an error")
}

/// romstage_native.c:native_restore_point() for a fast (cold power-on) boot:
/// the same controller setup as `train_cold` up to ODT, then the cached
/// registers and mode registers instead of training, then the JEDEC reset
/// replaying the trained mode registers. Normal mode is the caller's tail.
///
/// # Safety
/// All `train_cold` obligations apply. `record` must have been parsed for
/// this boot's identity (CPU, fuses, exact SPDs) and the same firmware.
pub unsafe fn restore_fast(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    policy: &ColdPolicy<'_>,
    state: &mut CalibrationState,
    record: &super::training_cache::Record,
    delay: &mut impl DelayNs,
) -> Result<(), ColdError> {
    let engine = Engine {
        registers,
        dimms: &policy.memory.dimms,
    };
    let mode = MemoryBootMode::Cold;
    let t = &policy.timings;
    let p = &policy.input;
    phase!(OverridesCapability, policy.revalidate());
    phase!(Pll, frequency::initialize(registers, t.clock, delay));
    phase!(
        Preflight,
        if state.dq_pat == super::io_test::DqPattern::Cadb as u8 {
            Err(Error::InvalidReutConfig)
        } else {
            Ok(())
        }
    );
    power_model::initialize_margin_limits(state);
    phase!(Clear, state.clear_cold_attempt(mode, 0, p.dummy_reads));
    phase!(
        Phy,
        phy::program_mc_registers(
            registers,
            delay,
            mode,
            p.cpu,
            &policy.memory,
            t,
            &p.phy,
            &mut state.training
        )
    );
    let base = phase!(
        BaseTiming,
        TimingRegisters::for_dimms(t, engine.dimms, p.cpu, p.dram_odt, [[0; 4]; 2])
    );
    base.program(registers, state);
    let mad = phase!(
        Mad,
        AddressDecode::for_dimms(
            engine.dimms,
            policy.overrides.rank_interleave,
            policy.overrides.enhanced_interleave,
            false
        )
    );
    mad.program(registers, delay);
    phase!(
        Odt,
        engine.meminit_power_optimization(state, mode, p.cpu.package, 8)
    );
    // Native runs pre-training with the restored mode registers, then
    // restores the trained registers over everything above.
    record.restore_state(state);
    phase!(
        PreTraining,
        engine.pre_training(mode, p.cpu.package, &state.saved_mr)
    );
    fstart_log::debug!("DDR4 training: Restore");
    record.restore_registers(registers);
    phase!(Rcomp, phy::force_rcomp(registers, delay));
    phase!(Preflight, super::memory_check::stopped(&engine));
    phase!(
        Restore,
        engine.reinitialize_cold(state, pmc, mode, 8, delay)
    );
    Ok(())
}

#[cfg(test)]
#[path = "cold_training_tests.rs"]
pub(crate) mod tests;
