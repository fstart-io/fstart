// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measured DDR4 optimizer from native/opt_param.c:train_ddr_opt_param.
use super::*;
use crate::skylake::{
    TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    io_test::{DqPattern, StopOnError},
    opt_tradeoff::{
        TradeoffConfig, TradeoffWorkspace, fill_2d_average, find_optimal_trade_off,
        running_average_2d,
    },
    phy::{PhyPolicy, force_rcomp},
    power_model::{
        CompSelection, LimitKind, MarginKind, Power, PowerCandidate, PowerModel, get_comp_code,
        margin_limit, normalize_power_to_margins,
    },
    registers::{ReutControl, ReutSequence},
    reut::{DownloadTarget, RegisterFile},
    reut_registers::CadbControl,
    training::cold_ranks,
};

#[derive(Debug, Clone, Copy)]
pub struct OptSweep<'a> {
    pub channels: u8,
    pub ranks: u8,
    pub parameters: &'a [OptParameter],
    pub grid: Grid,
    pub tests: &'a [MarginParameter],
    pub scale: [u8; 5],
    pub upm_optimize: [bool; 5],
    pub loop_count: u8,
    pub repeats: u8,
    /// Source skip_opt_update: measure/model the unchanged live baseline.
    pub skip_update: bool,
    pub read_read: Option<DqPattern>,
    pub guard_band: i16,
    pub mode: TrainingMode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptResult {
    pub grid: Grid,
    pub best: [[u16; 9]; 2],
    pub worst_width: [[u16; 2]; 4],
    pub channels: u8,
}

// Exact readable sequencer/setup words touched by VA setup, rank mapping, MR
// issue and BER. Status/counters and START are not replayable configuration.
#[derive(Clone, Copy)]
pub(crate) struct TestSnapshot {
    sequence: [u32; 2],
    subsequence: [u32; 8],
    start: u64,
    wrap: u64,
    order: u32,
    increment: u64,
    rank_map: u32,
    loops: u32,
    dummy_mask: u32,
    dummy_control: u32,
    error_control: u32,
    ecc_mask: u32,
    data_mask: u64,
    counters: [u32; 9],
    wdb: [u32; 3],
    cadb: [u32; 5],
    cke: u32,
    odt: u32,
}
impl TestSnapshot {
    pub(crate) const ZERO: Self = Self {
        sequence: [0; 2],
        subsequence: [0; 8],
        start: 0,
        wrap: 0,
        order: 0,
        increment: 0,
        rank_map: 0,
        loops: 0,
        dummy_mask: 0,
        dummy_control: 0,
        error_control: 0,
        ecc_mask: 0,
        data_mask: 0,
        counters: [0; 9],
        wdb: [0; 3],
        cadb: [0; 5],
        cke: 0,
        odt: 0,
    };
    pub(crate) fn load(e: &Engine<'_>, ch: usize) -> Result<Self, Error> {
        let r = e.registers;
        let c = &r.channel[ch];
        Ok(Self {
            sequence: core::array::from_fn(|i| r.reut_sequence[ch][i].get()),
            subsequence: core::array::from_fn(|i| {
                if ch == 0 {
                    r.reut_subsequence0[i].get()
                } else {
                    r.reut_subsequence1[i].get()
                }
            }),
            start: r.reut_address_start[ch].get(),
            wrap: r.reut_address_wrap[ch].get(),
            order: r.reut_address_order[ch].get(),
            increment: r.reut_address_increment[ch].get(),
            rank_map: r.reut_rank_map[ch].get(),
            loops: r.reut_loop_count[ch].get(),
            dummy_mask: r.reut_dummy_mask[ch].get(),
            dummy_control: r.reut_dummy_control[ch].get(),
            error_control: c.error_control.get(),
            ecc_mask: c.error_ecc_mask.get(),
            data_mask: c.error_data_mask.get(),
            counters: core::array::from_fn(|b| c.error_counter_control[b].get()),
            wdb: [c.wdb_control.get(), c.wdb_mux.get(), c.wdb_invert.get()],
            cadb: [
                c.cadb_control.get(),
                c.cadb_mrs.get(),
                c.cadb_loop_count.get(),
                c.cadb_write_pointer.get(),
                c.cadb_mux.get(),
            ],
            cke: c.cke.get(),
            odt: c.odt.get(),
        })
    }
    pub(crate) fn restore(&self, e: &Engine<'_>, ch: usize) {
        let r = e.registers;
        let c = &r.channel[ch];
        for i in 0..8 {
            if ch == 0 {
                r.reut_subsequence0[i].set(self.subsequence[i]);
            } else {
                r.reut_subsequence1[i].set(self.subsequence[i]);
            }
        }
        r.reut_address_start[ch].set(self.start);
        r.reut_address_wrap[ch].set(self.wrap);
        r.reut_address_order[ch].set(self.order);
        r.reut_address_increment[ch].set(self.increment);
        r.reut_rank_map[ch].set(self.rank_map);
        r.reut_loop_count[ch].set(self.loops);
        r.reut_dummy_mask[ch].set(self.dummy_mask);
        r.reut_dummy_control[ch].set(self.dummy_control);
        c.error_control.set(self.error_control);
        c.error_ecc_mask.set(self.ecc_mask);
        c.error_data_mask.set(self.data_mask);
        for b in 0..9 {
            c.error_counter_control[b].set(self.counters[b]);
        }
        c.wdb_control.set(self.wdb[0]);
        c.wdb_mux.set(self.wdb[1]);
        c.wdb_invert.set(self.wdb[2]);
        // MRS invalidates the write-only pattern FIFO. Do not replay its
        // pointer/loop metadata as if the former pattern remained owned.
        c.cadb_control
            .set(self.cadb[0] & !CadbControl::DESELECT::SET.value);
        c.cadb_mrs.set(0);
        c.cadb_loop_count.set(0);
        c.cadb_write_pointer.set(0);
        c.cadb_mux.set(self.cadb[4]);
        r.reut_control[ch].set(0);
        c.cke.set(self.cke);
        c.odt.set(self.odt);
        for i in 0..2 {
            r.reut_sequence[ch][i].set(self.sequence[i]);
        }
    }
}
struct OuterSnapshot {
    tests: [TestSnapshot; 2],
    rx: [[[u32; 9]; 4]; 2],
    tx: [[[u32; 9]; 4]; 2],
    controls: [[u32; 9]; 2],
    offsets: [[u32; 9]; 2],
    channel_controls: [u32; 2],
    host_comp: [[u32; 9]; 2],
    margins: [[[[u32; 8]; 9]; 4]; 2],
    misc: u32,
    comp2: u32,
    init: u32,
    seeds: [[u32; 11]; 2],
}
impl OuterSnapshot {
    const ZERO: Self = Self {
        tests: [TestSnapshot::ZERO; 2],
        rx: [[[0; 9]; 4]; 2],
        tx: [[[0; 9]; 4]; 2],
        controls: [[0; 9]; 2],
        offsets: [[0; 9]; 2],
        channel_controls: [0; 2],
        host_comp: [[0; 9]; 2],
        margins: [[[[0; 8]; 9]; 4]; 2],
        misc: 0,
        comp2: 0,
        init: 0,
        seeds: [[0; 11]; 2],
    };
    fn load(
        &mut self,
        e: &Engine<'_>,
        s: &CalibrationState,
        tests: &[MarginParameter],
    ) -> Result<(), Error> {
        for ch in 0..2 {
            self.tests[ch] = TestSnapshot::load(e, ch)?;
            let c = &e.registers.channel[ch];
            for i in 0..3 {
                self.seeds[ch][i] = c.wdb_read_seed[i].get();
                self.seeds[ch][i + 3] = c.wdb_write_seed[i].get();
                self.seeds[ch][i + 6] = c.cadb_seed[i].get();
            }
            self.seeds[ch][9] = c.wdb_lmn.get();
            self.seeds[ch][10] = c.cadb_lmn.get();
            self.channel_controls[ch] = e.registers.data_channel[ch].control0.get();
            for b in 0..9 {
                let lane = &e.registers.data_lane[b][ch];
                self.controls[ch][b] = lane.control0.get();
                self.offsets[ch][b] = lane.offset_train.get();
                self.host_comp[ch][b] = s.training.ch[ch].data_comp_offset[b];
                for rank in 0..4 {
                    self.rx[ch][rank][b] = lane.rx_train[rank].get();
                    self.tx[ch][rank][b] = lane.tx_train[rank].get();
                    for (i, p) in tests.iter().enumerate() {
                        self.margins[ch][rank][b][i * 2..i * 2 + 2].copy_from_slice(
                            &s.margin_results[ch][rank][b]
                                [p.result_type() * 2..p.result_type() * 2 + 2],
                        );
                    }
                }
            }
        }
        self.misc = e.registers.ddr_misc.get();
        self.comp2 = e.registers.comp_control2.get();
        self.init = e.registers.init_state.get();
        Ok(())
    }
}

/// Source major arrays plus independent outer snapshots. Caller owns this
/// workspace in CAR, not a temporary on a small stack. `size_of` is tested and
/// documented in the board checkpoint. No heap, global context or calibration
/// clone is used in production. Nested BER/reset frames are additional.
pub struct OptWorkspace {
    pub saved_widths: [[[[u16; 9]; 2]; MAX_OPT_POINTS]; 4],
    pub rows: [[u16; MAX_OPT_POINTS]; 5],
    pub scorer: TradeoffWorkspace,
    pub measured: [bool; MAX_OPT_POINTS],
    outer: OuterSnapshot,
}
impl OptWorkspace {
    pub const ZERO: Self = Self {
        saved_widths: [[[[0; 9]; 2]; MAX_OPT_POINTS]; 4],
        rows: [[0; MAX_OPT_POINTS]; 5],
        scorer: TradeoffWorkspace::ZERO,
        measured: [false; MAX_OPT_POINTS],
        outer: OuterSnapshot::ZERO,
    };
}

fn margin_kind(p: MarginParameter) -> Result<MarginKind, Error> {
    MarginKind::try_from(p as u8)
}
/// WRV publishes the joint measurement into every selected rank's separately
/// rebased PDA coordinates; other BER parameters publish the first rank slot.
/// Intersect those relative edges, and retain the first unusable-center rank
/// for final validation. These remain combined measurements, not rank-isolated
/// analog scans.
fn opt_measured_margin(
    state: &CalibrationState,
    parameter: MarginParameter,
    channel: usize,
    selected: u8,
    lane: usize,
) -> ([u32; 2], Option<usize>) {
    let outputs = if parameter == MarginParameter::WriteVoltage {
        selected
    } else {
        selected.isolate_lowest_one()
    };
    let mut common = [u32::MAX; 2];
    let mut zero_rank = None;
    let slot = parameter.result_type() * 2;
    for rank in 0..4 {
        if outputs & (1 << rank) == 0 {
            continue;
        }
        let edges = &state.margin_results[channel][rank][lane][slot..slot + 2];
        if edges.contains(&0) && zero_rank.is_none() {
            zero_rank = Some(rank);
        }
        for side in 0..2 {
            common[side] = common[side].min(edges[side]);
        }
    }
    (common, zero_rank)
}

fn comp_parameter(p: OptParameter) -> bool {
    matches!(
        p,
        OptParameter::WriteDrive
            | OptParameter::WriteDriveUpCoarse
            | OptParameter::WriteDriveDownCoarse
            | OptParameter::ReadOdt
            | OptParameter::Tco
            | OptParameter::Slew
    )
}
impl Engine<'_> {
    fn opt_apply(
        &self,
        s: &mut CalibrationState,
        h: &mut OptHost,
        package: Package,
        selected: [u8; 2],
        params: &[OptParameter],
        offsets: [i8; 2],
        lanes: usize,
        commit: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut candidate = *h;
        for (i, &p) in params.iter().enumerate() {
            for ch in 0..2 {
                if selected[ch] == 0 {
                    continue;
                }
                for b in 0..if p.channel_global() { 1 } else { lanes } {
                    self.update_opt_param_offset(
                        s,
                        &mut candidate,
                        package,
                        ch,
                        selected[ch],
                        b,
                        p,
                        i16::from(offsets[i]),
                        commit,
                        delay,
                    )?;
                    // Compose fields within a candidate, never across grid
                    // points. Two controls may share one hardware register.
                    candidate.comp[ch][b] = self.registers.data_lane[b][ch].offset_comp.get();
                    candidate.control1[ch][b] = self.registers.data_lane[b][ch].control1.get();
                    candidate.control2[ch][b] = self.registers.data_lane[b][ch].control2.get();
                }
                if p.channel_global() {
                    candidate.mr.rank[ch] = opt_mr_images(
                        self.dimms,
                        &h.mr,
                        ch,
                        selected[ch],
                        p,
                        i16::from(offsets[i]),
                    )?
                    .2;
                }
            }
        }
        // Nested BER must restore the candidate MR5/ODT, not erase it using
        // the outer saved image. The outer snapshot remains authoritative
        // for rollback and each candidate is derived afresh from h.
        s.saved_mr = candidate.mr;
        if commit {
            *h = candidate;
        }
        Ok(())
    }
    fn opt_measure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        selected: [u8; 2],
        p: MarginParameter,
        max: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if selected[ch] == 0 {
                continue;
            }
            let rank = if p == MarginParameter::WriteVoltage {
                selected[ch]
            } else {
                selected[ch].trailing_zeros() as u8
            };
            self.get_ber_margin_byte(
                s,
                ctx,
                BerSweep {
                    parameter: p,
                    channels: 1 << ch,
                    rank_in: rank,
                    rank_rx: rank,
                    per_channel: false,
                    interpolate: true,
                    max_margin: max,
                    interior: false,
                    byte_map: None,
                },
                &mut [0; 4],
                delay,
            )?;
        }
        Ok(())
    }
    fn opt_restore(
        &self,
        s: &mut CalibrationState,
        original: &OptHost,
        work: &OptWorkspace,
        cfg: &OptSweep<'_>,
        selected: [u8; 2],
        rollback: bool,
        comp_touched: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.stop(delay);
        let r = self.registers;
        let snap = &work.outer;
        let mut result = Ok(());
        if rollback {
            // Attempt every MR restore even after one transport has failed.
            {
                for ch in 0..2 {
                    for rank in 0..4 {
                        if selected[ch] == 0 || rank_mask(self.dimms, ch)? & (1 << rank) == 0 {
                            continue;
                        }
                        for mr in [1, 2, 5] {
                            let restore = self.training_mrs(
                                ch,
                                rank,
                                mr,
                                original.mr.rank[ch][rank][mr as usize],
                                delay,
                            );
                            if result.is_ok() {
                                result = restore;
                            }
                        }
                    }
                }
            }
            s.saved_mr = original.mr;
            for ch in 0..2 {
                for b in 0..9 {
                    s.training.ch[ch].data_comp_offset[b] = snap.host_comp[ch][b];
                    for rank in 0..4 {
                        for (i, p) in cfg.tests.iter().enumerate() {
                            s.margin_results[ch][rank][b]
                                [p.result_type() * 2..p.result_type() * 2 + 2]
                                .copy_from_slice(&snap.margins[ch][rank][b][i * 2..i * 2 + 2]);
                        }
                    }
                }
            }
            r.comp_control0.set(original.comp_control0);
            r.comp_control1.set(original.comp_control1);
            r.comp_control2.set(snap.comp2);
        }
        for ch in 0..2 {
            if rank_mask(self.dimms, ch)? == 0 {
                continue;
            }
            // Restore channel alias before the real lane register words.
            r.data_channel[ch].control0.set(snap.channel_controls[ch]);
            for b in 0..9 {
                let lane = &r.data_lane[b][ch];
                if rollback {
                    lane.offset_comp.set(original.comp[ch][b]);
                    lane.control1.set(original.control1[ch][b]);
                    lane.control2.set(original.control2[ch][b]);
                    for rank in 0..4 {
                        if rank_mask(self.dimms, ch)? & (1 << rank) == 0 {
                            continue;
                        }
                        lane.rx_train[rank].set(snap.rx[ch][rank][b]);
                        lane.tx_train[rank].set(snap.tx[ch][rank][b]);
                        let restore = self.download(
                            DownloadTarget::Lane {
                                channel: ch,
                                lane: b,
                            },
                            rank,
                            RegisterFile::Both,
                        );
                        if result.is_ok() {
                            result = restore;
                        }
                    }
                }
                lane.offset_train.set(snap.offsets[ch][b]);
                lane.control0.set(snap.controls[ch][b]);
            }
        }
        if rollback && comp_touched {
            r.ddr_misc
                .modify(DdrMisc::DATA_CLOCK_GATE_DISABLE::SET + DdrMisc::FORCE_COMP_UPDATE::SET);
            let restore = force_rcomp(r, delay);
            if result.is_ok() {
                result = restore;
            }
        }
        // Never replay START; all exits leave traffic stopped.
        r.ddr_misc.set(snap.misc);
        r.init_state.set(snap.init);
        for ch in 0..2 {
            snap.tests[ch].restore(self, ch);
            let c = &r.channel[ch];
            for i in 0..3 {
                c.wdb_read_seed[i].set(snap.seeds[ch][i]);
                c.wdb_write_seed[i].set(snap.seeds[ch][i + 3]);
                c.cadb_seed[i].set(snap.seeds[ch][i + 6]);
            }
            c.wdb_lmn.set(snap.seeds[ch][9]);
            c.cadb_lmn.set(snap.seeds[ch][10]);
        }
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
        result
    }

    /// `train_ddr_opt_param`: real source grid order, measured hardware BER
    /// and modeled electrical power. Selected settings are remeasured before
    /// publishing final edges; widths never become fabricated symmetric eyes.
    /// Requires exclusive, stopped test-engine ownership and no active CADB
    /// pattern ownership. Existing VA setup is performed here. MRS overwrites
    /// the write-only CADB FIFO: every exit invalidates its readable pointer/
    /// loop metadata and later CADB consumers MUST call setup_cadb/test again.
    /// Readable controls/host calibration restore, not old FIFO contents.
    pub fn train_ddr_opt_param(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        package: Package,
        phy: &PhyPolicy,
        cfg: &OptSweep<'_>,
        work: &mut OptWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let actual = cold_ranks(self, ctx.mode)?;
        if s.dq_pat == DqPattern::Cadb as u8
            || self
                .registers
                .reut_global_control
                .is_set(ReutControl::START)
            || (0..2).any(|ch| {
                self.registers.reut_control[ch].is_set(ReutControl::START)
                    || self.registers.channel[ch]
                        .cadb_control
                        .is_set(CadbControl::DESELECT)
                    || self.registers.reut_sequence[ch][0].read(ReutSequence::INIT_MODE) == 2
            })
        {
            return Err(Error::InvalidReutConfig);
        }
        let lanes = ctx.policy.sdram_lanes;
        if !matches!(lanes, 8 | 9)
            || cfg.channels == 0
            || cfg.channels & !3 != 0
            || cfg.ranks == 0
            || cfg.ranks & !15 != 0
            || !(1..=2).contains(&cfg.parameters.len())
            || cfg.parameters.len() != cfg.grid.dimensions
            || !(1..=4).contains(&cfg.tests.len())
            || cfg.repeats == 0
            || !cfg.scale.iter().any(|v| *v != 0)
            || cfg.parameters.iter().any(|p| !p.directly_programmable())
            || cfg
                .parameters
                .iter()
                .enumerate()
                .any(|(i, p)| cfg.parameters[..i].contains(p))
            || cfg.tests.iter().any(|p| {
                !matches!(
                    p,
                    MarginParameter::ReadTiming
                        | MarginParameter::ReadVoltage
                        | MarginParameter::WriteTiming
                        | MarginParameter::WriteVoltage
                        | MarginParameter::ReceiveEnableExtended
                )
            })
            || cfg
                .tests
                .iter()
                .enumerate()
                .any(|(i, p)| cfg.tests[..i].contains(p))
            || cfg.scale[cfg.tests.len() + 1..].iter().any(|v| *v != 0)
        {
            return Err(Error::InvalidPolicy);
        }
        if cfg.grid.mode == GridMode::CustomSlew && cfg.parameters != [OptParameter::Slew] {
            return Err(Error::InvalidPolicy);
        }
        // Mixing channel-global and independent-lane dimensions is not a
        // source DQ grid: it cannot produce mutually consistent lane winners.
        if cfg.parameters.iter().any(|p| p.channel_global()) && cfg.parameters.len() != 1 {
            return Err(Error::InvalidPolicy);
        }
        let selected = core::array::from_fn::<_, 2, _>(|ch| {
            if cfg.channels & (1 << ch) != 0 {
                actual[ch] & cfg.ranks
            } else {
                0
            }
        });
        let channels = u8::from(selected[0] != 0) | (u8::from(selected[1] != 0) << 1);
        if channels == 0 {
            return Err(Error::InvalidRank);
        }
        let mut grid = cfg.grid;
        grid.validate_axes()?;
        let mut comps = [0u8; 2];
        let mut need_comp = false;
        for (i, &p) in cfg.parameters.iter().enumerate() {
            if !cfg.skip_update && comp_parameter(p) {
                comps[i] = get_comp_code(self.registers, p, CompSelection::Combined, phy.vtt_odt)?;
                let coarse = matches!(
                    p,
                    OptParameter::WriteDriveUpCoarse | OptParameter::WriteDriveDownCoarse
                );
                let reserve = if coarse { 1 } else { 3 };
                let max = if coarse {
                    15
                } else if p == OptParameter::Slew {
                    31
                } else {
                    63
                };
                let low = i16::from(grid.start[i]).max(reserve - i16::from(comps[i]));
                let high = (i16::from(grid.start[i]) + grid.lengths[i] as i16 - 1)
                    .min(max - i16::from(comps[i]) - reserve);
                if high < low {
                    return Err(Error::InvalidPolicy);
                }
                grid.start[i] = i8::try_from(low).map_err(|_| Error::InvalidPolicy)?;
                grid.lengths[i] = (high - low + 1) as u16;
                need_comp = true;
            }
        }
        for (i, &p) in cfg.parameters.iter().enumerate() {
            if p == OptParameter::TxEq {
                comps[i] = get_comp_code(self.registers, p, CompSelection::Up, phy.vtt_odt)?;
            }
        }
        need_comp |= cfg.parameters.iter().any(|p| {
            matches!(
                p,
                OptParameter::WriteDriveUp
                    | OptParameter::WriteDriveDown
                    | OptParameter::ReadOdtUp
                    | OptParameter::ReadOdtDown
            )
        });
        if cfg.skip_update {
            grid.lengths = [1, 1];
            grid.mode = GridMode::Full;
            need_comp = false;
        }
        let points = grid.point_count()?;
        let mut upm = [0u16; 5];
        let mut max_margin = [0u8; 4];
        for (i, &p) in cfg.tests.iter().enumerate() {
            upm[i] = margin_limit(s, margin_kind(p)?, LimitKind::Upm, clock, cfg.mode)?;
            let power = margin_limit(s, margin_kind(p)?, LimitKind::Power, clock, cfg.mode)?;
            max_margin[i] = (power / 20).min(match p {
                MarginParameter::WriteVoltage => 36,
                MarginParameter::ReadVoltage | MarginParameter::ReceiveEnableExtended => 63,
                _ => 31,
            }) as u8;
        }
        // Validate electrical model resources before setting up any traffic.
        let first_ch = channels.trailing_zeros() as usize;
        PowerModel {
            registers: self.registers,
            dimms: self.dimms,
            saved_mr: &s.saved_mr,
            clock,
            package,
            phy,
        }
        .calc_cpu_impedance(
            RankLane {
                channel: first_ch,
                rank: selected[first_ch].trailing_zeros() as usize,
                lane: 0,
            },
            OptParameter::WriteDrive,
            None,
            None,
        )?;
        let subtract = match cfg.read_read {
            None => 0,
            Some(DqPattern::ReadRead) => 1,
            Some(DqPattern::ReadReadAll) => 3,
            _ => return Err(Error::InvalidPolicy),
        };
        let lc = cfg
            .loop_count
            .checked_sub(subtract)
            .ok_or(Error::InvalidPolicy)?;
        if !(1..=31).contains(&lc) {
            return Err(Error::InvalidPolicy);
        }
        let original = self.opt_host_sync(s, lanes)?;
        let mut host = original;
        work.outer.load(self, s, cfg.tests)?;
        work.measured.fill(false);
        let global = cfg.parameters[0].channel_global();
        let num_bytes = if global { 1 } else { lanes };
        let mut output = OptResult {
            grid,
            best: [[0; 9]; 2],
            worst_width: [[u16::MAX; 2]; 4],
            channels,
        };
        let result = (|| {
            self.setup_wdb(channels)?;
            self.setup_basic_va_test(s, channels, lc, 8, StopOnError::Never, delay)?;
            if let Some(p) = cfg.read_read {
                s.dq_pat = p as u8;
            }
            for ch in 0..2 {
                if selected[ch] != 0 {
                    self.program_rank_map(ch, selected[ch])?;
                }
            }
            for (test, &p) in cfg.tests.iter().enumerate() {
                for index in 0..points {
                    let (offsets, skip) = grid.get_params_xyz(index)?;
                    if skip {
                        continue;
                    }
                    if !cfg.skip_update {
                        self.opt_apply(
                            s,
                            &mut host,
                            package,
                            selected,
                            cfg.parameters,
                            offsets,
                            lanes,
                            false,
                            delay,
                        )?;
                    }
                    if need_comp {
                        force_rcomp(self.registers, delay)?;
                    }
                    for rep in 0..cfg.repeats {
                        self.opt_measure(s, ctx, selected, p, max_margin[test], delay)?;
                        for ch in 0..2 {
                            if selected[ch] == 0 {
                                continue;
                            }
                            let mut worst = u16::MAX;
                            for b in 0..lanes {
                                let (edges, _) = opt_measured_margin(s, p, ch, selected[ch], b);
                                let width = u16::try_from(
                                    edges[0].checked_add(edges[1]).ok_or(Error::InvalidPolicy)?,
                                )
                                .map_err(|_| Error::InvalidPolicy)?;
                                let cell = &mut work.saved_widths[test][index][ch][b];
                                *cell = if rep == 0 { width } else { (*cell).min(width) };
                                worst = worst.min(*cell);
                            }
                            if global {
                                work.saved_widths[test][index][ch][0] = worst;
                            }
                        }
                    }
                    work.measured[index] = true;
                }
            }
            for ch in 0..2 {
                if selected[ch] == 0 {
                    continue;
                }
                for b in 0..num_bytes {
                    for test in 0..cfg.tests.len() {
                        for index in 0..points {
                            work.rows[test][index] = if work.measured[index] {
                                work.saved_widths[test][index][ch][b]
                            } else {
                                0
                            };
                        }
                    }
                    let mut average = 1;
                    if grid.dimensions == 2 {
                        for test in 0..cfg.tests.len() {
                            if matches!(grid.mode, GridMode::ChessOdd | GridMode::ChessEven) {
                                fill_2d_average(
                                    &mut work.rows[test][..points],
                                    &work.measured[..points],
                                    grid.lengths[0] as usize,
                                    0,
                                    1,
                                    &mut work.scorer.scratch,
                                )?;
                            }
                            running_average_2d(
                                &mut work.rows[test][..points],
                                grid.lengths[0] as usize,
                                0,
                                1,
                                &mut work.scorer.scratch,
                            )?;
                        }
                    } else if !global {
                        if grid.mode == GridMode::Half {
                            for test in 0..cfg.tests.len() {
                                fill_2d_average(
                                    &mut work.rows[test][..points],
                                    &work.measured[..points],
                                    points,
                                    0,
                                    1,
                                    &mut work.scorer.scratch,
                                )?;
                            }
                        }
                        if cfg.parameters[0] == OptParameter::RxEq {
                            if points > 1 {
                                for test in 0..cfg.tests.len() {
                                    running_average_2d(
                                        &mut work.rows[test][1..points],
                                        4,
                                        0,
                                        test as u8,
                                        &mut work.scorer.scratch,
                                    )?;
                                }
                            }
                        } else {
                            average = if cfg.parameters[0] == OptParameter::RxBias {
                                3
                            } else {
                                7.min(points.saturating_sub(1)).max(1) as u8
                            };
                        }
                    }
                    // Model actual programmed candidates, not the last sweep's
                    // live EQ/ODT/control words masquerading as every point.
                    for index in 0..points {
                        let (offsets, _) = grid.get_params_xyz(index)?;
                        if !cfg.skip_update {
                            self.opt_apply(
                                s,
                                &mut host,
                                package,
                                selected,
                                cfg.parameters,
                                offsets,
                                lanes,
                                false,
                                delay,
                            )?;
                        }
                        if need_comp {
                            force_rcomp(self.registers, delay)?;
                        }
                        let mut candidates = [PowerCandidate {
                            parameter: cfg.parameters[0],
                            offset: 0,
                            compensation: 0,
                        }; 2];
                        for i in 0..cfg.parameters.len() {
                            candidates[i] = PowerCandidate {
                                parameter: cfg.parameters[i],
                                offset: offsets[i],
                                compensation: comps[i],
                            };
                        }
                        let mut power = Power::default();
                        PowerModel {
                            registers: self.registers,
                            dimms: self.dimms,
                            saved_mr: &s.saved_mr,
                            clock,
                            package,
                            phy,
                        }
                        .calc_opt_power_byte(
                            &mut power,
                            RankLane {
                                channel: ch,
                                rank: selected[ch].trailing_zeros() as usize,
                                lane: b,
                            },
                            &candidates[..if cfg.skip_update {
                                0
                            } else {
                                cfg.parameters.len()
                            }],
                            10,
                        )?;
                        let read = matches!(
                            cfg.parameters[0],
                            OptParameter::ReadOdt
                                | OptParameter::RxEq
                                | OptParameter::RxBias
                                | OptParameter::RxCb
                                | OptParameter::DimmRon
                        );
                        work.rows[cfg.tests.len()][index] =
                            u16::try_from(if read { power.read } else { power.write })
                                .map_err(|_| Error::InvalidPolicy)?;
                    }
                    normalize_power_to_margins(&mut work.rows, points, cfg.tests.len())?;
                    let gb =
                        if grid.dimensions == 2 && cfg.parameters[1] == OptParameter::WriteDrive {
                            cfg.guard_band
                                .checked_mul(grid.lengths[0] as i16)
                                .ok_or(Error::InvalidPolicy)?
                        } else {
                            cfg.guard_band
                        };
                    let scored = find_optimal_trade_off(
                        &work.rows,
                        points,
                        &TradeoffConfig {
                            scale: cfg.scale,
                            upm_optimize: cfg.upm_optimize,
                            square: false,
                            average,
                            include_ends: true,
                            center_scale: 1,
                            good_power_limit: upm,
                            optimization_mode: if cfg.scale[cfg.tests.len()] != 0 {
                                cfg.tests.len() as u8
                            } else {
                                100
                            },
                            guard_band: gb,
                            linear: false,
                        },
                        &mut work.scorer,
                    )?;
                    output.best[ch][b] = (scored.best as i16 + scored.guard_band) as u16;
                }
            }
            // Program all winners, then settle compensation once. For global
            // parameters every lane shares the channel's worst-lane winner.
            for ch in 0..2 {
                if selected[ch] == 0 {
                    continue;
                }
                s.training.ch[ch].data_comp_offset = original.comp[ch];
                for b in 0..num_bytes {
                    let (offsets, _) = grid.get_params_xyz(output.best[ch][b] as usize)?;
                    for (i, &p) in cfg.parameters.iter().enumerate() {
                        if cfg.skip_update {
                            continue;
                        }
                        self.update_opt_param_offset(
                            s,
                            &mut host,
                            package,
                            ch,
                            selected[ch],
                            b,
                            p,
                            i16::from(offsets[i]),
                            true,
                            delay,
                        )?;
                    }
                }
                if global {
                    let best = output.best[ch][0];
                    output.best[ch].fill(best);
                }
            }
            if need_comp {
                force_rcomp(self.registers, delay)?;
            }
            for (test, &p) in cfg.tests.iter().enumerate() {
                self.opt_measure(s, ctx, selected, p, max_margin[test], delay)?;
                for ch in 0..2 {
                    if selected[ch] == 0 {
                        continue;
                    }
                    for b in 0..lanes {
                        let (edges, zero_rank) = opt_measured_margin(s, p, ch, selected[ch], b);
                        if let Some(rank) = zero_rank {
                            return Err(Error::TrainingFailed {
                                step: TrainingStep::BerMargin,
                                channel: ch as u8,
                                rank: rank as u8,
                                lane: b as u8,
                            });
                        }
                        output.worst_width[test][ch] = output.worst_width[test][ch].min(
                            u16::try_from(
                                edges[0].checked_add(edges[1]).ok_or(Error::InvalidPolicy)?,
                            )
                            .map_err(|_| Error::InvalidPolicy)?,
                        );
                    }
                }
            }
            Ok(())
        })();
        let cleanup = self.opt_restore(
            s,
            &original,
            work,
            cfg,
            selected,
            result.is_err(),
            need_comp,
            delay,
        );
        cleanup?;
        result?;
        Ok(output)
    }

    /// First complete optimizer consumer, source `meminit_dimm_ron_training`.
    /// Callable only in the real DDR4 source order, after command DS/EQ/slew,
    /// normalization and baseline power meter. No dispatcher is wired here.
    pub fn meminit_dimm_ron_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        package: Package,
        phy: &PhyPolicy,
        mode: TrainingMode,
        work: &mut OptWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        self.train_ddr_opt_param(
            s,
            ctx,
            clock,
            package,
            phy,
            &OptSweep {
                channels: 3,
                ranks: 15,
                parameters: &[OptParameter::DimmRon],
                grid: Grid {
                    start: [0, 0],
                    lengths: [2, 1],
                    dimensions: 1,
                    mode: GridMode::Reversed1D,
                },
                tests: &[MarginParameter::ReadVoltage, MarginParameter::ReadTiming],
                scale: [1, 2, u8::from(mode == TrainingMode::Power), 0, 0],
                upm_optimize: [false; 5],
                loop_count: 15,
                repeats: 1,
                skip_update: false,
                read_read: None,
                guard_band: 0,
                mode,
            },
            work,
            delay,
        )
    }
}
