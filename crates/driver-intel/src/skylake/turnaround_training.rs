// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 cold turnaround consumers of native/si_optimization.c:1450–2030 and
//! additional_steps.c:180–714. Exclusive destructive WDB/CADB ownership; not a
//! dispatcher, normal-mode transition or certificate of usable memory.
use super::{
    Error, TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::{CommandContext, CommandIteration, PiSnapshot, pi},
    dq_si::stopped,
    io_test::{
        Address, AddressSetup, CommandPattern, DqPattern, StopOnError, TestSetup, WdbPattern,
    },
    opt_tradeoff::effective_margin,
    phy_registers::DataControl1,
    read_centering::ReadSnapshot,
    registers::{MrCommand, Pcit, ReutSequence},
    reut::{Engine, rank_mask},
    reut_registers::TestAddress,
    training::{channels, cold_ranks},
    training_state::CalibrationState,
    write_centering::WriteSnapshot,
};
use embedded_hal::delay::DelayNs;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

/// Rust-local discriminants, NOT native TA_OPTPARAM numeric IDs. Parameters
/// with no DDR4 setter in native dispatch (RDODTD/WRODTD) are not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TurnaroundParameter {
    DrReadRead,
    DdReadRead,
    DrWriteWrite,
    DdWriteWrite,
    DrReadWrite,
    DdReadWrite,
    DrWriteRead,
    DdWriteRead,
    SameWriteReadSg,
    SameWriteReadDg,
    SameReadWriteSg,
    SameReadWriteDg,
    McOdtDelay = 16,
    McOdtDuration = 17,
}
impl TurnaroundParameter {
    fn not_rank(self) -> bool {
        matches!(
            self,
            Self::SameWriteReadSg
                | Self::SameWriteReadDg
                | Self::SameReadWriteSg
                | Self::SameReadWriteDg
        )
    }
    fn per_byte(self) -> bool {
        matches!(self, Self::McOdtDelay | Self::McOdtDuration)
    }
    fn field(self) -> (usize, usize, i16) {
        use TurnaroundParameter::*;
        match self {
            DrReadRead => (0, 16, 63),
            DdReadRead => (0, 24, 63),
            DrWriteWrite => (3, 16, 63),
            DdWriteWrite => (3, 24, 63),
            DrReadWrite => (1, 16, 63),
            DdReadWrite => (1, 24, 63),
            DrWriteRead => (2, 16, 63),
            DdWriteRead => (2, 24, 63),
            SameWriteReadSg => (2, 0, 127),
            SameWriteReadDg => (2, 8, 63),
            SameReadWriteSg => (1, 0, 63),
            SameReadWriteDg => (1, 8, 63),
            _ => (0, 0, 0),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct CliffSearch<'a> {
    pub parameter: TurnaroundParameter,
    pub tests: &'a [MarginParameter],
    pub start: i16,
    pub stop: i16,
    pub loop_count: u8,
    pub clock_shifts: &'a [i16],
    pub rank_mask: u8,
    pub guard_band: u8,
    pub update_host: bool,
}

/// Independent outer rollback survives every nested BER/reset/MR helper. The
/// existing read/write snapshots are reused, not whole CalibrationState clones.
/// Allocate directly in caller CAR storage; nested BER frames are additional.
pub struct LatencyWorkspace {
    pub(crate) pi: [Option<PiSnapshot>; 2],
    read: ReadSnapshot,
    write: WriteSnapshot,
    timing: [[u32; 4]; 2],
    host_timing: [[u32; 2]; 2],
    control1: [[u32; 9]; 2],
    pub(crate) latency: [[u32; 2]; 2],
    pub(crate) host_latency: [[[u8; 2]; 4]; 2],
    extra_margins: [[[[u32; 4]; 9]; 4]; 2],
    bank: [[u32; 2]; 2],
    pcit: [u32; 2],
    pub(crate) baseline: [[u16; 8]; 2],
    margins: [[[[[u16; 8]; 2]; 2]; 2]; 6],
    pub(crate) measured: [[u16; 8]; 2],
    pub(crate) last: [[i16; 8]; 2],
    pub(crate) failed: [u8; 2],
    pub(crate) passed: [u8; 2],
    odt_host: [[u32; 8]; 2],
}
impl LatencyWorkspace {
    pub const INITIAL: Self = Self {
        pi: [None, None],
        read: ReadSnapshot::ZERO,
        write: WriteSnapshot::ZERO,
        timing: [[0; 4]; 2],
        host_timing: [[0; 2]; 2],
        control1: [[0; 9]; 2],
        latency: [[0; 2]; 2],
        host_latency: [[[0; 2]; 4]; 2],
        extra_margins: [[[[0; 4]; 9]; 4]; 2],
        bank: [[0; 2]; 2],
        pcit: [0; 2],
        baseline: [[0; 8]; 2],
        margins: [[[[[0; 8]; 2]; 2]; 2]; 6],
        measured: [[0; 8]; 2],
        last: [[0; 8]; 2],
        failed: [0; 2],
        passed: [0; 2],
        odt_host: [[0; 8]; 2],
    };
}

impl Default for LatencyWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
pub(crate) fn no_eye(ch: usize, rank: usize, lane: usize) -> Error {
    Error::TrainingFailed {
        step: TrainingStep::BerMargin,
        channel: ch as u8,
        rank: rank as u8,
        lane: lane as u8,
    }
}
fn timing_words(e: &Engine<'_>, ch: usize) -> [u32; 4] {
    let c = &e.registers.channel[ch];
    [
        c.tc_rdrd.get(),
        c.tc_rdwr.get(),
        c.tc_wrrd.get(),
        c.tc_wrwr.get(),
    ]
}
fn set_timing(e: &Engine<'_>, ch: usize, index: usize, word: u32) {
    let c = &e.registers.channel[ch];
    match index {
        0 => c.tc_rdrd.set(word),
        1 => c.tc_rdwr.set(word),
        2 => c.tc_wrrd.set(word),
        _ => c.tc_wrwr.set(word),
    }
}
pub(crate) fn merge(a: Result<(), Error>, b: Result<(), Error>) -> Result<(), Error> {
    match (a, b) {
        (Err(Error::TrainingFailed { .. }), Err(e)) => Err(e),
        (Ok(()), b) => b,
        (a, _) => a,
    }
}
impl Engine<'_> {
    pub(crate) fn latency_preflight(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
    ) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        self.validate_write_centering(s, ctx, channels(ranks))?;
        stopped(self, s)?;
        // Independent PDA/MR command ownership is not covered by REUT/CADB.
        if self
            .registers
            .channel
            .iter()
            .any(|channel| channel.mr_command.is_set(MrCommand::BUSY))
        {
            return Err(Error::InvalidReutConfig);
        }
        Ok(ranks)
    }
    pub(crate) fn latency_begin(
        &self,
        s: &CalibrationState,
        w: &mut LatencyWorkspace,
    ) -> Result<(), Error> {
        self.read_snapshot(s, &mut w.read)?;
        self.write_snapshot(s, &mut w.write)?;
        for ch in 0..2 {
            w.pi[ch] = Some(PiSnapshot::load(self, s, ch));
            w.timing[ch] = timing_words(self, ch);
            w.host_timing[ch] = [s.mchbar_tc_rdrd[ch], s.mchbar_tc_rdwr[ch]];
            let c = &self.registers.channel[ch];
            w.latency[ch] = [c.roundtrip_latency.get(), c.io_latency.get()];
            w.pcit[ch] = c.pcit.get();
            for i in 0..2 {
                w.bank[ch][i] = self.registers.reut_bank_map[ch][i].get();
            }
            for b in 0..9 {
                w.control1[ch][b] = self.registers.data_lane[b][ch].control1.get();
                if b < 8 {
                    w.odt_host[ch][b] = w.control1[ch][b];
                }
            }
            for r in 0..4 {
                let t = &s.training.ch[ch].rank[r];
                w.host_latency[ch][r] = [t.rt_latency, t.io_latency];
                for b in 0..9 {
                    w.extra_margins[ch][r][b].copy_from_slice(&s.margin_results[ch][r][b][8..12]);
                }
            }
        }
        Ok(())
    }
    pub(crate) fn latency_finish(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        w: &LatencyWorkspace,
        measured: Result<(), Error>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut cleanup = self.read_restore(s, ranks, &w.read, measured.is_err(), delay);
        cleanup = merge(
            cleanup,
            self.restore_write_centering(
                s,
                channels(ranks),
                ranks,
                &w.write,
                measured.is_err(),
                delay,
            ),
        );
        // A cleanup failure rolls back even after an apparently good measurement.
        let rollback = measured.is_err() || cleanup.is_err();
        if rollback && measured.is_ok() {
            cleanup = merge(cleanup, self.read_restore(s, ranks, &w.read, true, delay));
            cleanup = merge(
                cleanup,
                self.restore_write_centering(s, channels(ranks), ranks, &w.write, true, delay),
            );
        }
        for ch in 0..2 {
            if ranks[ch] != 0 {
                cleanup = merge(
                    cleanup,
                    w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?.restore(
                        self,
                        s,
                        ch,
                        ranks[ch],
                        ctx.policy.sdram_lanes,
                        rollback,
                    ),
                );
            }
            for i in 0..2 {
                self.registers.reut_bank_map[ch][i].set(w.bank[ch][i]);
            }
            self.registers.channel[ch].pcit.set(w.pcit[ch]);
            if rollback {
                for i in 0..4 {
                    set_timing(self, ch, i, w.timing[ch][i]);
                }
                s.mchbar_tc_rdrd[ch] = w.host_timing[ch][0];
                s.mchbar_tc_rdwr[ch] = w.host_timing[ch][1];
                self.registers.channel[ch]
                    .roundtrip_latency
                    .set(w.latency[ch][0]);
                self.registers.channel[ch].io_latency.set(w.latency[ch][1]);
                for r in 0..4 {
                    s.training.ch[ch].rank[r].rt_latency = w.host_latency[ch][r][0];
                    s.training.ch[ch].rank[r].io_latency = w.host_latency[ch][r][1];
                    for b in 0..9 {
                        s.margin_results[ch][r][b][8..12]
                            .copy_from_slice(&w.extra_margins[ch][r][b]);
                    }
                }
                for b in 0..9 {
                    self.registers.data_lane[b][ch]
                        .control1
                        .set(w.control1[ch][b]);
                }
            }
        }
        // PiSnapshot restoration persistently downloads RFs, rewriting these
        // selectors after read/write cleanup. Restore broadcast first, then
        // each independent unicast image, on success and every failure exit.
        for ch in 0..2 {
            self.registers.data_channel[ch]
                .control0
                .set(w.read.channel_controls[ch]);
            for byte in 0..9 {
                self.registers.data_lane[byte][ch]
                    .control0
                    .set(w.read.controls[ch][byte]);
            }
        }
        merge(measured, cleanup)
    }
    pub(crate) fn latency_stress(&self, ranks: [u8; 2]) {
        for ch in 0..2 {
            if ranks[ch] != 0 {
                self.registers.channel[ch].pcit.modify(Pcit::CPU.val(255));
            }
        }
    }
    pub(crate) fn latency_bank_groups(&self, selected: u8) {
        for ch in 0..2 {
            if selected & (1 << ch) == 0 {
                continue;
            }
            let min_banks = self.dimms[ch]
                .iter()
                .flatten()
                .map(|d| u16::from(d.bank_groups) * u16::from(d.banks))
                .min()
                .unwrap_or(0);
            let map = if min_banks == 16 {
                0x0000_0cc8_8440_c840u64
            } else {
                0x0000_0440_0440_4040
            };
            self.registers.reut_bank_map[ch][0].set(map as u32);
            self.registers.reut_bank_map[ch][1].set((map >> 32) as u32);
        }
    }
    pub(crate) fn latency_setup(
        &self,
        s: &mut CalibrationState,
        selected: u8,
        parameter: Option<TurnaroundParameter>,
        lc: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        use TurnaroundParameter::*;
        let mut setup = TestSetup {
            command: CommandPattern::WriteRead,
            cachelines: 128,
            loop_exponent: lc,
            address: AddressSetup {
                start: Address::default(),
                end: Address {
                    rank: 7,
                    column: 1023,
                    ..Address::default()
                },
                order: [0; 4],
                increment_rate: [0; 4],
                increment: [1, 0, 0, 1],
            },
            stop: StopOnError::Never,
            wdb: WdbPattern {
                increment_rate: 13,
                start: 0,
                end: 7,
                pattern: DqPattern::Turnaround,
            },
            cadb: false,
            cke_wait: false,
            subsequence_wait: 0,
            qclk_ps: 0,
        };
        match parameter {
            None => {
                setup.cachelines = 4;
                setup.wdb.increment_rate = 16;
                setup.wdb.pattern = DqPattern::ReadRead;
                setup.address.end.bank = 3;
                setup.address.end.column = 1016;
                setup.address.increment[1] = 1;
                self.latency_bank_groups(selected);
            }
            Some(p @ (DrWriteRead | DdWriteRead | SameWriteReadSg | SameWriteReadDg)) => {
                setup.command = CommandPattern::WriteReadTurnaround;
                setup.address.end.row = 1024;
                setup.wdb.pattern = match p {
                    SameWriteReadSg => DqPattern::TurnaroundWrSg,
                    SameWriteReadDg => DqPattern::TurnaroundWrDg,
                    _ => DqPattern::TurnaroundWr,
                };
            }
            Some(p @ (DrReadWrite | DdReadWrite | SameReadWriteSg | SameReadWriteDg)) => {
                setup.command = CommandPattern::ReadWriteTurnaround;
                setup.cachelines = 512;
                setup.address.end.row = 1024;
                setup.address.increment_rate[2] = 128;
                setup.address.increment[2] = 1;
                let inc = if p.not_rank() { 31 } else { 1 };
                setup.address.increment_rate[0] = inc;
                setup.address.increment_rate[3] = inc;
                setup.wdb.pattern = match p {
                    SameReadWriteSg => DqPattern::TurnaroundRwSg,
                    SameReadWriteDg => DqPattern::TurnaroundRwDg,
                    _ => DqPattern::TurnaroundRw,
                };
            }
            Some(McOdtDelay | McOdtDuration) => {
                setup.command = CommandPattern::OdtTurnaround;
                setup.wdb.pattern = DqPattern::TurnaroundOdt;
                setup.address.increment_rate[0] = 1;
                setup.address.increment_rate[3] = 1;
            }
            _ => {}
        }
        if let Some(p) = parameter {
            if matches!(
                p,
                SameReadWriteDg | SameWriteReadDg | McOdtDelay | McOdtDuration
            ) {
                setup.address.end.bank = 3;
                setup.address.increment[1] = 1;
                self.latency_bank_groups(selected);
            } else if matches!(p, SameReadWriteSg | SameWriteReadSg) {
                for ch in 0..2 {
                    if selected & (1 << ch) != 0 {
                        self.registers.reut_bank_map[ch][0].set(0x2110_3210);
                        self.registers.reut_bank_map[ch][1].set(0x332);
                    }
                }
            }
        }
        self.setup_io_test(s, selected, &setup, delay)?;
        s.dq_pat = setup.wdb.pattern as u8;
        let shift = i32::from(lc) - super::io_test::ceil_log2(setup.cachelines)? as i32;
        s.dq_pat_lc = 1u16
            .checked_shl(shift.max(0) as u32)
            .ok_or(Error::InvalidPolicy)?;
        Ok(())
    }
    pub(crate) fn latency_select(
        &self,
        masks: [u8; 2],
        selected: u8,
        kind: Option<usize>,
    ) -> Result<(), Error> {
        const MAPPING: [usize; 16] = [15, 15, 15, 4, 15, 3, 15, 1, 15, 15, 15, 15, 5, 2, 15, 0];
        const ORDER: [[u32; 6]; 2] = [
            [0x32320101, 0x20101010, 0x23232320, 0x20, 0x10, 0x23],
            [0x21303120, 0x2120, 0x3020, 0x20, 0, 0],
        ];
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0]
                .modify(ReutSequence::GLOBAL_CONTROL.val(u32::from(selected & (1 << ch) != 0)));
            if selected & (1 << ch) == 0 {
                continue;
            }
            if masks[ch] == 0 {
                return Err(Error::InvalidRank);
            }
            let mut list = match kind {
                Some(dd) if MAPPING[usize::from(masks[ch])] != 15 => {
                    ORDER[dd][MAPPING[usize::from(masks[ch])]]
                }
                _ => 0x3210,
            };
            let mut map = 0;
            let mut count = 0;
            for _ in 0..8 {
                if list == 0 {
                    break;
                }
                let r = list & 15;
                list >>= 4;
                if masks[ch] & (1 << r) != 0 {
                    map |= r << (count * 4);
                    count += 1;
                }
            }
            if count == 0 {
                return Err(Error::InvalidRank);
            }
            self.registers.reut_rank_map[ch].set(map);
            self.registers.reut_address_wrap[ch].modify(TestAddress::RANK.val(count - 1));
        }
        Ok(())
    }
    pub(crate) fn latency_unshift(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        w: &LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut result = Ok(());
        for ch in 0..2 {
            if ranks[ch] != 0 {
                result = merge(
                    result,
                    w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?.restore(
                        self,
                        s,
                        ch,
                        rank_mask(self.dimms, ch)?,
                        8,
                        false,
                    ),
                );
            }
        }
        merge(result, self.command_reset(s, ctx, delay))
    }
    pub(crate) fn latency_shift_channel(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        masks: [u8; 2],
        w: &LatencyWorkspace,
        requested: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if masks[ch] == 0 {
                continue;
            }
            let p = w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?;
            let coupled = if ctx.policy.ddp_shared_clock && masks[ch] & 3 != 0 {
                masks[ch] | (rank_mask(self.dimms, ch)? & 3)
            } else {
                masks[ch]
            };
            let (_, dqmax) = snapshot_dq_limits(p, coupled);
            let mut cmdmax = pi(p.cke, 0).max(pi(p.cmds, 0)).max(pi(p.cmdn, 0));
            for r in 0..4 {
                if masks[ch] & (1 << r) != 0 {
                    cmdmax = cmdmax.max(pi(p.ctl, r));
                    if r < 3 {
                        cmdmax = cmdmax.max(pi(p.cke_ctl, r));
                    }
                }
            }
            let offset = requested.min(511 - dqmax).min(127 - cmdmax).max(0);
            self.shift_command_pi(
                s,
                p,
                ch,
                masks[ch],
                CommandIteration::Clock,
                offset,
                false,
                ctx.policy,
            )?;
            // Source uses the first selected CTL code for all selected ranks.
            self.shift_command_pi(
                s,
                p,
                ch,
                masks[ch],
                CommandIteration::Control,
                pi(p.ctl, masks[ch].trailing_zeros() as usize) + offset,
                false,
                ctx.policy,
            )?;
            for (iter, word) in [
                (CommandIteration::CmdSouth, p.cmds),
                (CommandIteration::CmdNorth, p.cmdn),
                (CommandIteration::Cke, p.cke),
            ] {
                self.shift_command_pi(
                    s,
                    p,
                    ch,
                    masks[ch],
                    iter,
                    pi(word, 0) + offset,
                    false,
                    ctx.policy,
                )?;
            }
        }
        self.command_reset(s, ctx, delay)
    }
    fn cliff_clock_phase(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        masks: [u8; 2],
        selected: u8,
        dd: usize,
        shift: i16,
        w: &LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if dd == 0 && ctx.policy.ddp_shared_clock {
            return Ok(());
        }
        let groups = if dd == 1 { [3, 12] } else { [5, 10] };
        for (g, sign) in [(groups[0], 1), (groups[1], -1)] {
            let mut low = -511;
            let mut high = 511;
            for ch in 0..2 {
                if selected & (1 << ch) == 0 || masks[ch] & g == 0 {
                    continue;
                }
                let (min, max) = snapshot_dq_limits(
                    w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?,
                    if ctx.policy.ddp_shared_clock && masks[ch] & g & 3 != 0 {
                        (masks[ch] & g) | (rank_mask(self.dimms, ch)? & 3)
                    } else {
                        masks[ch] & g
                    },
                );
                low = low.max(-min);
                high = high.min(511 - max);
            }
            // Negative headroom must stay negative: never native's unsigned
            // underflow/sign-loss when limiting a negative phase.
            let offset = (sign * shift).clamp(low, high);
            for ch in 0..2 {
                if selected & (1 << ch) != 0 && masks[ch] & g != 0 {
                    self.shift_command_pi(
                        s,
                        w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?,
                        ch,
                        masks[ch] & g,
                        CommandIteration::Clock,
                        offset,
                        false,
                        ctx.policy,
                    )?;
                }
            }
            self.command_reset(s, ctx, delay)?;
        }
        Ok(())
    }
    fn cliff_masks(&self, ranks: [u8; 2], cfg: &CliffSearch<'_>) -> [u8; 2] {
        let mut drdd = [0; 2];
        for ch in 0..2 {
            let m = ranks[ch] & cfg.rank_mask;
            if m == 0 {
                continue;
            }
            let dr = m & 3 == 3 || m & 12 == 12;
            let dd = m & 3 != 0 && m & 12 != 0;
            if cfg.parameter.not_rank()
                || cfg.parameter.per_byte()
                || ((cfg.parameter as u8) & 1 == 0 && dr)
            {
                drdd[0] |= 1 << ch;
            }
            if (cfg.parameter as u8) < 8 && (cfg.parameter as u8) & 1 != 0 && dd {
                drdd[1] |= 1 << ch;
            }
        }
        drdd
    }
    fn validate_cliff(&self, cfg: &CliffSearch<'_>) -> Result<(), Error> {
        if cfg.tests.is_empty()
            || cfg.tests.len() > 6
            || cfg.clock_shifts.is_empty()
            || cfg.clock_shifts.len() > 2
            || cfg.clock_shifts.iter().any(|s| !(-127..=127).contains(s))
            || cfg.rank_mask == 0
            || cfg.rank_mask & !15 != 0
            || !(1..=16).contains(&cfg.loop_count)
            || cfg.start >= cfg.stop
            || cfg.tests.iter().any(|p| {
                !matches!(
                    p,
                    MarginParameter::ReadTiming
                        | MarginParameter::ReadVoltage
                        | MarginParameter::WriteTiming
                        | MarginParameter::WriteVoltage
                        | MarginParameter::ReceiveEnableExtended
                        | MarginParameter::WriteLevel
                )
            })
        {
            return Err(Error::InvalidPolicy);
        }
        let (lo, hi) = if cfg.parameter.per_byte() {
            (-8, 7)
        } else {
            (0, cfg.parameter.field().2)
        };
        if cfg.start < lo
            || cfg.stop > hi
            || cfg.guard_band > 7
            || (cfg.parameter == TurnaroundParameter::McOdtDelay
                && cfg.start - i16::from(cfg.guard_band) < lo)
            || (cfg.parameter != TurnaroundParameter::McOdtDelay
                && cfg.stop + i16::from(cfg.guard_band) > hi)
        {
            return Err(Error::InvalidTiming);
        }
        Ok(())
    }
    fn cliff_set(
        &self,
        s: &mut CalibrationState,
        ch: usize,
        p: TurnaroundParameter,
        off: i16,
        byte: usize,
        w: &LatencyWorkspace,
        update: bool,
    ) -> Result<(), Error> {
        if p.per_byte() {
            let mut v = LocalRegisterCopy::<u32, DataControl1::Register>::new(w.odt_host[ch][byte]);
            if p == TurnaroundParameter::McOdtDelay {
                let base = v.read(DataControl1::ODT_DELAY) as i16;
                let base = if base & 8 != 0 { base - 16 } else { base };
                let n = ((base + off).clamp(-4, 6) as u32) & 15;
                v.modify(DataControl1::ODT_DELAY.val(n) + DataControl1::SENSE_AMP_DELAY.val(n));
            } else {
                let n = (v.read(DataControl1::ODT_DURATION) as i16 + off).clamp(0, 7) as u32;
                v.modify(
                    DataControl1::ODT_DURATION.val(n) + DataControl1::SENSE_AMP_DURATION.val(n),
                );
            }
            self.registers.data_lane[byte][ch].control1.set(v.get());
        } else {
            let (idx, shift, max) = p.field();
            if !(0..=max).contains(&off) {
                return Err(Error::InvalidTiming);
            }
            let base = if idx == 0 {
                s.mchbar_tc_rdrd[ch]
            } else {
                timing_words(self, ch)[idx]
            };
            let word = (base & !((max as u32) << shift)) | ((off as u32) << shift);
            set_timing(self, ch, idx, word);
            if update {
                if idx == 0 {
                    s.mchbar_tc_rdrd[ch] = word;
                }
                if idx == 1 {
                    s.mchbar_tc_rdwr[ch] = word;
                }
            }
        }
        Ok(())
    }
    /// Native ta_cliff_measure: seed the BER walk with the minimum previous
    /// edges, then measure the existing REUT rank sequence once. In contrast
    /// to RTL training, do not download/sweep each rank's RF in isolation.
    fn cliff_measure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        masks: [u8; 2],
        selected: u8,
        param: MarginParameter,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        use super::ber_margin::{
            MAX_POSSIBLE_DDR4_WRITE_VREF, MAX_POSSIBLE_TIME, MAX_POSSIBLE_VREF,
        };
        let wrv = param == MarginParameter::WriteVoltage;
        let union = (0..2)
            .filter(|&ch| selected & (1 << ch) != 0)
            .fold(0, |all, ch| all | masks[ch]);
        let max_margin = match param {
            MarginParameter::WriteVoltage => MAX_POSSIBLE_DDR4_WRITE_VREF,
            MarginParameter::ReadVoltage => MAX_POSSIBLE_VREF,
            _ => MAX_POSSIBLE_TIME,
        };
        let i = param.result_type() * 2;
        for ch in 0..2 {
            if selected & (1 << ch) == 0 {
                continue;
            }
            let present = rank_mask(self.dimms, ch)?;
            let result_rank = masks[ch].trailing_zeros() as usize;
            for byte in 0..8 {
                for edge in i..i + 2 {
                    let seed = (0..4)
                        .filter(|&rank| present & (1 << rank) != 0)
                        .map(|rank| s.margin_results[ch][rank][byte][edge])
                        .min()
                        .ok_or(Error::InvalidRank)?;
                    s.margin_results[ch][result_rank][byte][edge] = seed;
                }
            }
        }
        // The native M900 uses result slot 0 on both populated channels.
        // Group by first selected rank for sparse populations without changing
        // the logical-to-physical rank map installed by latency_select().
        for rank in 0..if wrv { 1 } else { 4 } {
            let channels = if wrv {
                selected
            } else {
                (0..2).fold(0, |mask, ch| {
                    mask | if selected & (1 << ch) != 0 && masks[ch].trailing_zeros() == rank {
                        1 << ch
                    } else {
                        0
                    }
                })
            };
            if channels == 0 {
                continue;
            }
            self.get_ber_margin_byte(
                s,
                ctx,
                BerSweep {
                    parameter: param,
                    channels,
                    rank_in: if wrv { union } else { rank as u8 },
                    rank_rx: if wrv { union } else { 0xff },
                    per_channel: false,
                    interpolate: true,
                    max_margin,
                    interior: false,
                    byte_map: None,
                },
                &mut [0; 4],
                delay,
            )?;
        }
        for ch in 0..2 {
            if selected & (1 << ch) == 0 {
                continue;
            }
            let rank = masks[ch].trailing_zeros() as usize;
            for byte in 0..8 {
                let edge = &s.margin_results[ch][rank][byte][i..i + 2];
                w.measured[ch][byte] = effective_margin(
                    u16::try_from(edge[0]).map_err(|_| Error::InvalidTiming)?,
                    u16::try_from(edge[1]).map_err(|_| Error::InvalidTiming)?,
                )?;
            }
        }
        Ok(())
    }

    fn cliff_value(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: &CliffSearch<'_>,
        masks: [u8; 2],
        drdd: [u8; 2],
        w: &mut LatencyWorkspace,
        index: usize,
        off: i16,
        delay: &mut impl DelayNs,
    ) -> Result<bool, Error> {
        let inc = if cfg.parameter == TurnaroundParameter::McOdtDelay {
            1
        } else {
            -1
        };
        let guard = inc * i16::from(cfg.guard_band);
        for dd in 0..2 {
            let selected = drdd[dd];
            if selected == 0 {
                continue;
            }
            self.latency_select(
                masks,
                selected,
                if cfg.parameter.not_rank() {
                    None
                } else {
                    Some(dd)
                },
            )?;
            for (phase, &shift) in cfg.clock_shifts.iter().enumerate() {
                let result = (|| {
                    if !cfg.parameter.not_rank() {
                        self.cliff_clock_phase(s, ctx, masks, selected, dd, shift, w, delay)?;
                    }
                    for (test, &param) in cfg.tests.iter().enumerate() {
                        let done = (0..2).all(|ch| {
                            selected & (1 << ch) == 0
                                || if cfg.parameter.per_byte() {
                                    w.failed[ch] == u8::MAX
                                } else {
                                    w.failed[ch] != 0
                                }
                        });
                        if done {
                            return Ok(true);
                        }
                        self.cliff_measure(s, ctx, masks, selected, param, w, delay)?;
                        for ch in 0..2 {
                            if selected & (1 << ch) == 0 {
                                continue;
                            }
                            for b in 0..8 {
                                let m = w.measured[ch][b];
                                let base = &mut w.margins[test][dd][phase][ch][b];
                                if index == 0 {
                                    if m <= 20 {
                                        fstart_log::error!(
                                            "TAT {:?}: {:?} baseline CH{} byte{} margin={} at offset {} shift {}",
                                            fstart_log::DebugValue(&cfg.parameter),
                                            fstart_log::DebugValue(&param),
                                            ch,
                                            b,
                                            m,
                                            off,
                                            shift,
                                        );
                                        return Err(no_eye(
                                            ch,
                                            masks[ch].trailing_zeros() as usize,
                                            b,
                                        ));
                                    }
                                    *base = (*base).min(m);
                                } else if w.failed[ch] & (1 << b) == 0 {
                                    if u32::from(m.max(20)) < u32::from(*base) * 90 / 100 {
                                        w.failed[ch] |= 1 << b;
                                        w.last[ch][b] = off - inc - guard;
                                    } else {
                                        w.last[ch][b] = off - guard;
                                    }
                                }
                            }
                        }
                    }
                    Ok(false)
                })();
                let cleanup = if !cfg.parameter.not_rank() {
                    self.latency_unshift(s, ctx, masks, w, delay)
                } else {
                    Ok(())
                };
                let done = result.as_ref().is_ok_and(|&done| done);
                merge(result.map(drop), cleanup)?;
                if done {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    fn cliff_inner(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        cfg: &CliffSearch<'_>,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<[Option<i16>; 2], Error> {
        // Each native cliff call owns a bank-map override independently of
        // the outer TAT transaction. Later dr/dd tests need their entry map,
        // not the preceding SG/DG stress map.
        let bank = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 2, _>(|i| self.registers.reut_bank_map[ch][i].get())
        });
        let result = self.cliff_search_body(s, ctx, ranks, cfg, w, delay);
        for ch in 0..2 {
            for i in 0..2 {
                self.registers.reut_bank_map[ch][i].set(bank[ch][i]);
            }
        }
        result
    }
    fn cliff_search_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        cfg: &CliffSearch<'_>,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<[Option<i16>; 2], Error> {
        let drdd = self.cliff_masks(ranks, cfg);
        let selected = drdd[0] | drdd[1];
        if selected == 0 {
            return Err(Error::InvalidRank);
        }
        let masks = core::array::from_fn(|ch| {
            if selected & (1 << ch) != 0 {
                ranks[ch] & cfg.rank_mask
            } else {
                0
            }
        });
        self.latency_setup(s, selected, Some(cfg.parameter), cfg.loop_count, delay)?;
        let inc = if cfg.parameter == TurnaroundParameter::McOdtDelay {
            1
        } else {
            -1
        };
        let begin = if inc == 1 { cfg.start } else { cfg.stop };
        w.last = [[begin - inc * i16::from(cfg.guard_band); 8]; 2];
        w.failed = [0; 2];
        w.margins = [[[[[1280; 8]; 2]; 2]; 2]; 6];
        for index in 0..=(cfg.stop - cfg.start) as usize {
            let off = begin + inc * index as i16;
            for ch in 0..2 {
                if masks[ch] == 0 {
                    continue;
                }
                for b in 0..if cfg.parameter.per_byte() { 8 } else { 1 } {
                    self.cliff_set(s, ch, cfg.parameter, off, b, w, false)?;
                }
            }
            if self.cliff_value(s, ctx, cfg, masks, drdd, w, index, off, delay)? {
                break;
            }
        }
        let mut best = [None; 2];
        for ch in 0..2 {
            if masks[ch] == 0 {
                continue;
            }
            if !cfg.update_host {
                w.last[ch] = [begin; 8];
            }
            let winner = if inc == 1 {
                *w.last[ch].iter().min().ok_or(Error::InvalidPolicy)?
            } else {
                *w.last[ch].iter().max().ok_or(Error::InvalidPolicy)?
            };
            let (lo, hi) = if cfg.parameter.per_byte() {
                (-8, 7)
            } else {
                (0, cfg.parameter.field().2)
            };
            if !(lo..=hi).contains(&winner) {
                return Err(Error::InvalidTiming);
            }
            best[ch] = Some(winner);
            for b in 0..if cfg.parameter.per_byte() { 8 } else { 1 } {
                let v = if cfg.parameter.per_byte() {
                    w.last[ch][b]
                } else {
                    winner
                };
                self.cliff_set(s, ch, cfg.parameter, v, b, w, cfg.update_host)?;
            }
        }
        if cfg.parameter.per_byte() && cfg.update_host {
            for ch in 0..2 {
                if masks[ch] != 0 {
                    for b in 0..8 {
                        w.odt_host[ch][b] = self.registers.data_lane[b][ch].control1.get();
                    }
                }
            }
        }
        Ok(best)
    }
    /// Callable native meminit_ta_opt_param_cliff, real DDR4 supported dispatch.
    pub fn meminit_ta_opt_param_cliff(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: CliffSearch<'_>,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<[Option<i16>; 2], Error> {
        self.validate_cliff(&cfg)?;
        let ranks = self.latency_preflight(s, ctx)?;
        if self.cliff_masks(ranks, &cfg) == [0; 2] {
            return Err(Error::InvalidRank);
        }
        self.latency_begin(s, w)?;
        let result = (|| {
            self.setup_wdb(channels(ranks))?;
            s.en_dummy_reads = 0;
            self.cliff_inner(s, ctx, ranks, &cfg, w, delay)
        })();
        let finish = self.latency_finish(
            s,
            ctx,
            ranks,
            w,
            result.as_ref().map(|_| ()).map_err(|e| *e),
            delay,
        );
        finish.and(result)
    }
    /// Native additional_steps.c meminit_turnaround_timing, in source call order.
    /// Invoke after round-trip latency and before write/read 2D; no enable-bit
    /// policy or skipped mandatory algorithm is invented here.
    pub fn meminit_turnaround_timing(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        qclk_ps: u32,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        use MarginParameter::*;
        use TurnaroundParameter::*;
        let ranks = self.latency_preflight(s, ctx)?;
        // Native QCLK is half the selected DRAM period, not a free-standing
        // delay. Reuse the canonical clock table and actual PCODE acceptance.
        let clock = super::timings::select_clock(
            qclk_ps.checked_mul(2000).ok_or(Error::InvalidTiming)?,
            super::timings::ReferenceClock::Mhz133,
        )
        .map_err(|_| Error::InvalidTiming)?;
        if clock.tck_fs / 2000 != qclk_ps {
            return Err(Error::InvalidTiming);
        }
        super::frequency::validate_running(self.registers, clock)?;
        if (0..2).any(|ch| {
            (0..4).any(|r| {
                ranks[ch] & (1 << r) != 0
                    && s.training.ch[ch].rank[r].rcven_dly[..8]
                        .iter()
                        .any(|v| *v > 511)
            })
        }) {
            return Err(Error::InvalidTiming);
        }
        const ALL: [MarginParameter; 6] = [
            ReadVoltage,
            ReadTiming,
            WriteVoltage,
            WriteTiming,
            ReceiveEnableExtended,
            WriteLevel,
        ];
        const RD: [MarginParameter; 3] = [ReadVoltage, ReadTiming, ReceiveEnableExtended];
        const WR: [MarginParameter; 2] = [WriteVoltage, WriteTiming];
        let mut nominal = [0i16; 12];
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            let mut words = timing_words(self, ch);
            words[0] = s.mchbar_tc_rdrd[ch];
            for p in [
                DrReadRead,
                DdReadRead,
                DrWriteWrite,
                DdWriteWrite,
                DrReadWrite,
                DdReadWrite,
                DrWriteRead,
                DdWriteRead,
                SameWriteReadSg,
                SameWriteReadDg,
                SameReadWriteSg,
                SameReadWriteDg,
            ] {
                let (idx, shift, max) = p.field();
                nominal[p as usize] =
                    nominal[p as usize].max(((words[idx] >> shift) & max as u32) as i16);
            }
        }
        // Validate every source range before PCIT, WDB, MR or controller writes.
        let order = [
            SameWriteReadSg,
            SameReadWriteSg,
            SameWriteReadDg,
            SameReadWriteDg,
            DdReadRead,
            DdWriteWrite,
            DdWriteRead,
            DdReadWrite,
            DrReadRead,
            DrWriteWrite,
            DrWriteRead,
            DrReadWrite,
            McOdtDelay,
            McOdtDuration,
        ];
        for &p in &order {
            let cfg = wrapper_config(p, &nominal, &ALL, &RD, &WR);
            if self.cliff_masks(ranks, &cfg) != [0; 2] {
                self.validate_cliff(&cfg)?;
            }
        }
        self.latency_begin(s, w)?;
        let measured = (|| {
            self.setup_wdb(channels(ranks))?;
            s.en_dummy_reads = 0;
            self.latency_stress(ranks);
            for &p in &order {
                if p == McOdtDelay {
                    self.sample_odt(ranks, qclk_ps, w)?;
                }
                let cfg = wrapper_config(p, &nominal, &ALL, &RD, &WR);
                if self.cliff_masks(ranks, &cfg) == [0; 2] {
                    continue;
                }
                self.cliff_inner(s, ctx, ranks, &cfg, w, delay)?;
            }
            Ok(())
        })();
        self.latency_finish(s, ctx, ranks, w, measured, delay)
    }
    fn sample_odt(
        &self,
        ranks: [u8; 2],
        qclk_ps: u32,
        w: &mut LatencyWorkspace,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            for b in 0..8 {
                let mut min = 512;
                let mut max = 0;
                for r in 0..4 {
                    if ranks[ch] & (1 << r) != 0 {
                        let v = (w.pi[ch].as_ref().ok_or(Error::InvalidPolicy)?.host_rcven[r][b])
                            as i32;
                        min = min.min(v);
                        max = max.max(v);
                    }
                }
                let wake = (80000 / qclk_ps).max(128) as i32;
                let on = (((min - wake) >> 6) - 1).clamp(-4, 6);
                let duration = ((max >> 6) + 1 - on - 1 + 1).clamp(0, 7);
                let v = &self.registers.data_lane[b][ch].control1;
                v.modify(
                    DataControl1::ODT_DELAY.val((on as u32) & 15)
                        + DataControl1::SENSE_AMP_DELAY.val((on as u32) & 15)
                        + DataControl1::ODT_DURATION.val(duration as u32)
                        + DataControl1::SENSE_AMP_DURATION.val(duration as u32),
                );
                w.odt_host[ch][b] = v.get();
            }
        }
        Ok(())
    }
}
fn snapshot_dq_limits(p: &PiSnapshot, ranks: u8) -> (i16, i16) {
    let mut min = 511;
    let mut max = 0;
    for r in 0..4 {
        if ranks & (1 << r) != 0 {
            for b in 0..8 {
                for v in [p.tx[r][b] & 511, (p.tx[r][b] >> 9) & 511, p.rx[r][b] & 511] {
                    min = min.min(v as i16);
                    max = max.max(v as i16);
                }
            }
        }
    }
    (min, max)
}
fn wrapper_config<'a>(
    p: TurnaroundParameter,
    n: &[i16; 12],
    all: &'a [MarginParameter],
    rd: &'a [MarginParameter],
    wr: &'a [MarginParameter],
) -> CliffSearch<'a> {
    use TurnaroundParameter::*;
    let (start, stop) = match p {
        McOdtDelay => (0, 3),
        McOdtDuration => (-2, 0),
        DrReadRead | DdReadRead => (n[p as usize], n[p as usize] + 1),
        DrWriteRead | DdWriteRead => ((n[p as usize] - 1).max(4), n[p as usize] + 1),
        _ => (n[p as usize] - 1, n[p as usize] + 1),
    };
    CliffSearch {
        parameter: p,
        tests: match p {
            DrReadRead | DdReadRead | McOdtDelay | McOdtDuration => rd,
            DrWriteWrite | DdWriteWrite => wr,
            _ => all,
        },
        start,
        stop,
        loop_count: 12,
        clock_shifts: if p.not_rank() { &[0] } else { &[-7, 7] },
        rank_mask: 15,
        guard_band: u8::from(p.per_byte()),
        update_host: true,
    }
}
