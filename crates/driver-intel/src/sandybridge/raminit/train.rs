//! DRAM training algorithms (coreboot `raminit_common.c`).

use super::{
    BROADCAST, CCC_MAX_PI, NUM_CHANNELS, NUM_LANES, NUM_SLOTRANKS, QCLK_PI, Raminit, RankTimings,
    TrainError, TrainResult, iosav, tables,
};
use crate::sandybridge::regs::{MC_INIT_STATE_G, SCHED_CBIT, TC_RWP, TRAINING_MOD};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

const MAX_EDGE_TIMING: i32 = 71;
const MAX_TX_DQ: i32 = 127;
const RCVEN_COARSE_PI_LENGTH: usize = 2 * QCLK_PI as usize;
const TX_DQS_PI_LENGTH: usize = 2 * QCLK_PI as usize;
const CT_MIN_PI: i32 = -CCC_MAX_PI;
const CT_MAX_PI: i32 = CCC_MAX_PI + 1;
const CT_PI_LENGTH: usize = (CT_MAX_PI - CT_MIN_PI + 1) as usize;
const MIN_C320C_LEN: i32 = 13;
/// coreboot uses 4 of the vendor BIOS's 64 aggressive-training patterns.
const NUM_PATTERNS: usize = 4;
/// Training patterns go to DRAM at 64 MiB, before anything is loaded there.
const PATTERN_BASE: usize = 0x0400_0000;

const fn words(bits: usize) -> usize {
    bits.div_ceil(32)
}

/// The longest run of zero bits in a circular bitmap (coreboot `struct run`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    middle: i32,
    end: i32,
    start: i32,
    all: bool,
    length: i32,
}

fn bit(seq: &[u32], i: usize) -> bool {
    seq[i / 32] & (1 << (i % 32)) != 0
}

fn set_bit(seq: &mut [u32], off: usize, value: bool) {
    if value {
        seq[off / 32] |= 1 << (off % 32);
    } else {
        seq[off / 32] &= !(1 << (off % 32));
    }
}

/// coreboot `get_longest_zero_run_bm`.
fn longest_zero_run(seq: &[u32], sz: usize) -> Run {
    let (mut bl, mut bs, mut ls) = (0usize, 0usize, 0usize);
    for i in 0..2 * sz {
        if bit(seq, i % sz) {
            if i - ls > bl {
                bl = i - ls;
                bs = ls;
            }
            ls = i + 1;
        }
    }
    if bl == 0 {
        return Run {
            middle: (sz / 2) as i32,
            start: 0,
            end: sz as i32,
            length: sz as i32,
            all: true,
        };
    }
    Run {
        start: (bs % sz) as i32,
        end: ((bs + bl - 1) % sz) as i32,
        middle: ((bs + (bl - 1) / 2) % sz) as i32,
        length: bl as i32,
        all: false,
    }
}

fn sfence() {
    // SAFETY: a store fence has no memory-safety preconditions.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    unsafe {
        core::arch::asm!("sfence", options(nostack, preserves_flags))
    };
}

fn write_dram32(addr: usize, value: u32) {
    // SAFETY: the pattern window at 64 MiB is DRAM the controller just
    // mapped; nothing is loaded there before training completes.
    unsafe { fstart_core::mmio::write32(addr as *mut u32, value) };
}

impl Raminit {
    fn err(channel: usize) -> TrainResult {
        Err(TrainError { channel })
    }

    fn lane_works(&self, ch: usize, rank: usize, lane: usize) -> bool {
        let rcven = self.c.timings[ch][rank].lanes[lane].rcven as u32;
        let result = self.mch.lane(ch, lane).training_result[((rcven / 32) & 1) as usize].get();
        (result >> (rcven % 32)) & 1 != 0
    }

    fn test_rcven(&mut self, ch: usize, rank: usize) {
        self.io.wait(ch);
        let (t_mod, cas) = (self.c.t_mod, self.c.cas);
        // A burst of 16 back-to-back reads, 4 DCLK apart.
        self.io.read_mpr(ch, rank as u32, t_mod, 1, 3, 15, cas + 36);
        self.io.run_once_and_wait(ch);
    }

    fn find_rcven_pi_coarse(&mut self, ch: usize, rank: usize, upper: &mut [i32; NUM_LANES]) {
        let mut stats = [[0u32; words(RCVEN_COARSE_PI_LENGTH)]; NUM_LANES];
        for rcven in 0..RCVEN_COARSE_PI_LENGTH {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].rcven = rcven as i32;
            }
            self.program_timings(ch);
            self.test_rcven(ch, rank);
            for lane in 0..self.c.lanes {
                let failed = !self.lane_works(ch, rank, lane);
                set_bit(&mut stats[lane], rcven, failed);
            }
        }
        for lane in 0..self.c.lanes {
            let run = longest_zero_run(&stats[lane], RCVEN_COARSE_PI_LENGTH);
            self.c.timings[ch][rank].lanes[lane].rcven = run.middle;
            upper[lane] = run.end;
            if upper[lane] < run.middle {
                upper[lane] += 2 * QCLK_PI;
            }
        }
    }

    fn fine_tune_rcven_pi(&mut self, ch: usize, rank: usize, upper: &[i32; NUM_LANES]) {
        let mut stats = [[0i32; 51]; NUM_LANES];
        for delta in -25..=25 {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].rcven = upper[lane] + delta + QCLK_PI;
            }
            self.program_timings(ch);
            for _ in 0..100 {
                self.test_rcven(ch, rank);
                for lane in 0..self.c.lanes {
                    stats[lane][(delta + 25) as usize] +=
                        i32::from(self.lane_works(ch, rank, lane));
                }
            }
        }
        for lane in 0..self.c.lanes {
            let last_zero = (-25..=25)
                .find(|d: &i32| stats[lane][(d + 25) as usize] != 0)
                .unwrap_or(26)
                - 1;
            let first_all = (-25..=25)
                .find(|d: &i32| stats[lane][(d + 25) as usize] == 100)
                .unwrap_or(26);
            self.c.timings[ch][rank].lanes[lane].rcven = (last_zero + first_all) / 2 + upper[lane];
        }
    }

    /// Find the round-trip latency by sampling the strobe on the preamble.
    fn find_roundtrip_latency(
        &mut self,
        ch: usize,
        rank: usize,
        upper: &mut [i32; NUM_LANES],
    ) -> TrainResult {
        loop {
            self.program_timings(ch);
            self.test_rcven(ch, rank);
            let mut works = [false; NUM_LANES];
            for (lane, works) in works.iter_mut().enumerate().take(self.c.lanes) {
                *works = !self.lane_works(ch, rank, lane);
            }
            let n = self.c.lanes;
            if works[..n].iter().all(|w| *w) {
                return Ok(());
            }
            let t = &mut self.c.timings[ch][rank];
            if !works[..n].iter().any(|w| *w) {
                if t.roundtrip_latency < 2 {
                    fstart_log::error!("sandybridge: roundtrip latency underflow {} {}", ch, rank);
                    return Self::err(ch);
                }
                t.roundtrip_latency -= 2;
                continue;
            }
            t.io_latency += 2;
            if t.io_latency >= 16 {
                fstart_log::error!("sandybridge: I/O latency overflow {} {}", ch, rank);
                return Self::err(ch);
            }
            for lane in 0..n {
                if works[lane] {
                    t.lanes[lane].rcven += 2 * QCLK_PI;
                    upper[lane] += 2 * QCLK_PI;
                }
            }
        }
    }

    fn logic_delay_delta(&self, ch: usize, rank: usize) -> i32 {
        let lanes = &self.c.timings[ch][rank].lanes[..self.c.lanes];
        let min = lanes.iter().map(|l| l.rcven >> 6).fold(7, i32::min);
        let max = lanes.iter().map(|l| l.rcven >> 6).fold(0, i32::max);
        if max < min {
            fstart_log::error!("sandybridge: logic delay max < min {} {}", ch, rank);
        }
        max - min
    }

    fn align_rt_io_latency(&mut self, ch: usize, rank: usize, prev: i32) -> i32 {
        let post = self.logic_delay_delta(ch, rank);
        let offset = (post - prev).signum();
        let t = &mut self.c.timings[ch][rank];
        t.io_latency += offset;
        t.roundtrip_latency += offset;
        post
    }

    fn compute_final_logic_delay(&mut self, ch: usize, rank: usize) {
        let lanes = self.c.lanes;
        let t = &mut self.c.timings[ch][rank];
        let min = t.lanes[..lanes]
            .iter()
            .map(|l| l.rcven >> 6)
            .fold(7, i32::min);
        if min >= 2 {
            fstart_log::warn!(
                "sandybridge: logic delay {} greater than 1: {} {}",
                min,
                ch,
                rank
            );
        }
        for lane in &mut t.lanes[..lanes] {
            lane.rcven -= min << 6;
        }
        t.io_latency -= min;
    }

    /// coreboot `receive_enable_calibration`.
    pub(super) fn receive_enable_calibration(&mut self) -> TrainResult {
        for (ch, rank) in self.c.all_ranks() {
            let mut upper = [0i32; NUM_LANES];
            self.io.wait(ch);
            let t_rp = self.c.t_rp;
            self.io.prea(ch, rank as u32, t_rp, 0);
            self.io.run_once_and_wait(ch);
            self.mch.training_mod.write(
                TRAINING_MOD::RECEIVE_ENABLE_MODE::SET
                    + TRAINING_MOD::TRAINING_RANK_SEL.val(rank as u32)
                    + TRAINING_MOD::ODT_ALWAYS_ON::SET,
            );
            self.c.rank(ch, rank).io_latency = 4;
            self.c.rank(ch, rank).roundtrip_latency = 55;
            self.program_timings(ch);
            self.find_rcven_pi_coarse(ch, rank, &mut upper);

            let lanes = self.c.lanes;
            let high = self.c.timings[ch][rank].lanes[..lanes]
                .iter()
                .filter(|l| l.rcven >= QCLK_PI)
                .count();
            if high == lanes {
                let t = self.c.rank(ch, rank);
                t.io_latency -= 1;
                for (lane, upper) in t.lanes[..lanes].iter_mut().zip(upper.iter_mut()) {
                    lane.rcven -= QCLK_PI;
                    *upper -= QCLK_PI;
                }
            } else if high > 0 {
                let t = self.c.rank(ch, rank);
                t.roundtrip_latency += 1;
                t.io_latency += 1;
            }
            self.program_timings(ch);
            let prev = self.logic_delay_delta(ch, rank);
            self.find_roundtrip_latency(ch, rank, &mut upper)?;
            let prev = self.align_rt_io_latency(ch, rank, prev);
            self.fine_tune_rcven_pi(ch, rank, &upper);
            let prev = self.align_rt_io_latency(ch, rank, prev);
            self.compute_final_logic_delay(ch, rank);
            self.align_rt_io_latency(ch, rank, prev);
            fstart_log::debug!(
                "sandybridge: rcven {} {}: roundtrip {} io {}",
                ch,
                rank,
                self.c.timings[ch][rank].roundtrip_latency,
                self.c.timings[ch][rank].io_latency
            );
            self.mch.training_mod.set(0);
            self.toggle_io_reset();
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        Ok(())
    }

    fn test_tx_dq(&mut self, ch: usize, rank: usize) {
        for lane in 0..self.c.lanes {
            self.mch.mc[ch].error_count[lane].set(0);
            let _ = self.mch.mc[ch].bw_serror_c[lane].get();
        }
        self.io.wait(ch);
        let c = self.c;
        let gap = c.t_rrd.max((c.t_faw >> 2) + 1);
        self.io.misc_write(&c, ch, rank as u32, gap, 4, 4, 500, 18);
        self.io.run_once_and_wait(ch);
        self.io.prea_act_read(&c, ch, rank as u32);
        self.io.run_once_and_wait(ch);
    }

    fn tx_dq_write_leveling(&mut self, ch: usize, rank: usize) -> TrainResult {
        const LEN: usize = MAX_TX_DQ as usize + 1;
        let mut stats = [[0u32; words(LEN)]; NUM_LANES];
        let mut stat_min = [u16::MAX; NUM_LANES];
        let mut stat_max = [0u16; NUM_LANES];
        self.io.wait(ch);
        let t_rp = self.c.t_rp;
        self.io.prea(ch, rank as u32, t_rp, 18);
        self.io.run_once_and_wait(ch);
        for tx_dq in 0..=MAX_TX_DQ {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].tx_dq = tx_dq;
            }
            self.program_timings(ch);
            self.test_tx_dq(ch, rank);
            for lane in 0..self.c.lanes {
                let stat = self.mch.mc[ch].error_count[lane].get() as u16;
                stat_min[lane] = stat_min[lane].min(stat);
                stat_max[lane] = stat_max[lane].max(stat);
                set_bit(&mut stats[lane], tx_dq as usize, stat != 0);
            }
        }
        for lane in 0..self.c.lanes {
            let mut run = longest_zero_run(&stats[lane], LEN);
            if run.all || run.length < 8 {
                fstart_log::error!(
                    "sandybridge: tx_dq write leveling failed {} {} {}",
                    ch,
                    rank,
                    lane
                );
                // Without command training the lane can be erroneous: retry
                // against the average error count.
                let threshold = u32::from(stat_min[lane] / 2 + stat_max[lane] / 2);
                for tx_dq in 0..=MAX_TX_DQ {
                    self.c.timings[ch][rank].lanes[lane].tx_dq = tx_dq;
                    self.program_timings(ch);
                    self.test_tx_dq(ch, rank);
                    let errors = self.mch.mc[ch].error_count[lane].get();
                    set_bit(&mut stats[lane], tx_dq as usize, errors > threshold);
                }
                run = longest_zero_run(&stats[lane], LEN);
                if run.all || run.length < 8 {
                    fstart_log::error!("sandybridge: tx_dq recovery failed");
                    return Self::err(ch);
                }
            }
            self.c.timings[ch][rank].lanes[lane].tx_dq = run.middle;
        }
        Ok(())
    }

    fn preceding_channels(&self, target: usize) -> usize {
        self.c
            .populated_channels()
            .filter(|&ch| ch < target)
            .count()
    }

    fn num_channels(&self) -> usize {
        self.c.populated_channels().count()
    }

    /// Each cache line is 64 bits: IOSAV_DATA_CTL[7:0] = lines / 8 - 1.
    fn program_wdb_pattern_length(&self, ch: usize, lines: u32) {
        // coreboot writes only the low byte, leaving the WDB pointers.
        let register = &self.mch.mc[ch].data_ctl as *const _ as *mut u8;
        // SAFETY: byte 0 of IOSAV_DATA_CTL inside the mapped MCHBAR.
        unsafe { fstart_core::mmio::write8(register, (lines / 8 - 1) as u8) };
    }

    fn fill_pattern0(&self, ch: usize, a: u32, b: u32) {
        let offset = self.preceding_channels(ch) * 64;
        for j in 0..16 {
            write_dram32(
                PATTERN_BASE + offset + 4 * j,
                if j & 2 != 0 { b } else { a },
            );
        }
        sfence();
        self.program_wdb_pattern_length(ch, 8);
    }

    fn fill_pattern1(&self, ch: usize) {
        let offset = self.preceding_channels(ch) * 64;
        let step = 64 * self.num_channels();
        for j in 0..16 {
            write_dram32(PATTERN_BASE + offset + j * 4, 0xffff_ffff);
        }
        for j in 0..16 {
            write_dram32(PATTERN_BASE + offset + step + j * 4, 0);
        }
        sfence();
        self.program_wdb_pattern_length(ch, 16);
    }

    fn fill_pattern5(&self, ch: usize, patno: usize) {
        let offset = self.preceding_channels(ch) * 64;
        let step = 64 * self.num_channels();
        if patno != 0 {
            let base8 = 0x80u32 >> ((patno - 1) % 8);
            let base = base8 | base8 << 8 | base8 << 16 | base8 << 24;
            for i in 0..32 {
                for j in 0..16 {
                    let mut val = if tables::USE_BASE[patno - 1][i] & (1 << (j / 2)) != 0 {
                        base
                    } else {
                        0
                    };
                    if tables::INVERT[patno - 1][i] & (1 << (j / 2)) != 0 {
                        val = !val;
                    }
                    write_dram32(PATTERN_BASE + offset + i * step + j * 4, val);
                }
            }
        } else {
            for (i, row) in tables::PATTERN.iter().enumerate() {
                for (j, &val) in row.iter().enumerate() {
                    write_dram32(PATTERN_BASE + offset + i * step + j * 4, val);
                }
            }
            sfence();
        }
        self.program_wdb_pattern_length(ch, 256);
    }

    fn write_level_rank(&mut self, ch: usize, rank: usize) -> TrainResult {
        let mut stats = [[0u32; words(TX_DQS_PI_LENGTH)]; NUM_LANES];
        self.mch.training_mod.write(
            TRAINING_MOD::WRITE_LEVELING_MODE::SET
                + TRAINING_MOD::TRAINING_RANK_SEL.val(rank as u32)
                + TRAINING_MOD::ENABLE_DQS_WL.val(5)
                + TRAINING_MOD::ODT_ALWAYS_ON::SET
                + TRAINING_MOD::FORCE_DRIVE_ENABLE::SET,
        );
        let (mut bank, mut mr1) = (1, self.make_mr1(ch) | 1 << 7);
        if self.c.rank_mirror[ch][rank] {
            (bank, mr1) = super::mirror_mrreg(bank, mr1);
        }
        self.io.wait(ch);
        let c = self.c;
        self.io.jedec_write_leveling(&c, ch, rank as u32, bank, mr1);
        for tx_dqs in 0..TX_DQS_PI_LENGTH {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].tx_dqs = tx_dqs as i32;
            }
            self.program_timings(ch);
            self.io.run_once_and_wait(ch);
            if (tx_dqs + 1) % QCLK_PI as usize == 0 {
                let qclk = tx_dqs / QCLK_PI as usize;
                for (lane, stats) in stats.iter_mut().enumerate().take(self.c.lanes) {
                    let regs = self.mch.lane(ch, lane);
                    stats[2 * qclk] = !regs.training_result[0].get();
                    stats[2 * qclk + 1] = !regs.training_result[1].get();
                }
            }
        }
        for lane in 0..self.c.lanes {
            let mut run = longest_zero_run(&stats[lane], TX_DQS_PI_LENGTH);
            // tx_dq derives from the 6 LSBs of tx_dqs; avoid an overflow when
            // later tests nudge tx_dqs upwards.
            match run.start & 0x3f {
                0x3e => run.start += 2,
                0x3f => run.start += 1,
                _ => {}
            }
            self.c.timings[ch][rank].lanes[lane].tx_dqs = run.start;
            if run.all {
                fstart_log::error!(
                    "sandybridge: JEDEC write leveling failed {} {} {}",
                    ch,
                    rank,
                    lane
                );
                return Self::err(ch);
            }
        }
        Ok(())
    }

    fn disable_refresh_machine(&mut self) {
        for ch in self.c.populated_channels() {
            let rank = if self.c.rankmap[ch] & 1 == 0 { 2 } else { 0 };
            self.io.zqcs(ch, rank, 4, 4, 31);
            self.io.run_once_and_wait(ch);
            self.mch.mc[ch]
                .sched_cbit
                .modify(SCHED_CBIT::DIS_REFRESH::SET);
        }
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::REFRESH_ENABLE::CLEAR);
        for ch in self.c.populated_channels() {
            self.io.run_once_and_wait(ch);
        }
    }

    /// coreboot `jedec_write_leveling`.
    fn jedec_write_leveling(&mut self) -> TrainResult {
        self.disable_refresh_machine();
        for (ch, rank) in self.c.all_ranks() {
            let mr1 = self.make_mr1(ch) | 1 << 12 | 1 << 7;
            self.write_mrreg(ch, rank, 1, mr1);
        }
        self.mch.training_mod.write(
            TRAINING_MOD::WRITE_LEVELING_MODE::SET
                + TRAINING_MOD::ENABLE_DQS_WL.val(5)
                + TRAINING_MOD::ODT_ALWAYS_ON::SET
                + TRAINING_MOD::FORCE_DRIVE_ENABLE::SET,
        );
        self.toggle_io_reset();
        for (ch, rank) in self.c.all_ranks() {
            self.write_level_rank(ch, rank)?;
        }
        for (ch, rank) in self.c.all_ranks() {
            let mr1 = self.make_mr1(ch);
            self.write_mrreg(ch, rank, 1, mr1);
        }
        self.mch.training_mod.set(0);
        for ch in self.c.populated_channels() {
            self.io.wait(ch);
        }
        self.mch
            .mc_init_state_g
            .modify(MC_INIT_STATE_G::REFRESH_ENABLE::SET);
        for ch in self.c.populated_channels() {
            self.mch.mc[ch]
                .sched_cbit
                .modify(SCHED_CBIT::DIS_REFRESH::CLEAR);
            let _ = self.mch.mc[ch].status.get();
            self.io.wait(ch);
            self.io.zqcs(ch, 0, 4, 101, 31);
            self.io.run_once_and_wait(ch);
        }
        self.toggle_io_reset();
        Ok(())
    }

    fn train_write_flyby(&mut self) {
        self.mch
            .training_mod
            .write(TRAINING_MOD::DQ_DQS_TRAINING_RES::SET);
        for ch in self.c.populated_channels() {
            self.fill_pattern1(ch);
        }
        for ch in self.c.populated_channels() {
            for rank in self.c.populated_ranks(ch) {
                // Reset read and write WDB pointers.
                self.mch.mc[ch].data_ctl.set(0x10001);
                self.io.wait(ch);
                let c = self.c;
                self.io.misc_write(&c, ch, rank as u32, 3, 1, 3, 3, 31);
                self.io.run_once_and_wait(ch);
                let t = c.timings[ch][rank];
                let r = rank as u32;
                self.io.write(
                    ch,
                    &[
                        iosav::Ssq {
                            post: c.t_rp,
                            address: 1 << 10,
                            update: iosav::Update::wrap(18),
                            ..iosav::Ssq::new(iosav::PRE, r)
                        },
                        iosav::Ssq {
                            post: c.t_rcd,
                            ..iosav::Ssq::new(iosav::ACT, r)
                        },
                        iosav::Ssq {
                            ranksel_ap: 3,
                            post: (c.t_rp as i32 + t.roundtrip_latency + t.io_latency) as u32,
                            dir: iosav::Dir::Read,
                            address: 8,
                            ..iosav::Ssq::new(iosav::RD, r)
                        },
                    ],
                );
                self.io.run_once_and_wait(ch);
                for lane in 0..self.c.lanes {
                    let regs = self.mch.lane(ch, lane);
                    let res = u64::from(regs.training_result[0].get())
                        | u64::from(regs.training_result[1].get()) << 32;
                    self.c.timings[ch][rank].lanes[lane].tx_dqs += dqs_flyby_adjust(res) * QCLK_PI;
                }
            }
        }
        self.mch.training_mod.set(0);
    }

    /// coreboot `write_training`.
    pub(super) fn write_training(&mut self) -> TrainResult {
        // DEC_WRD is needed by the write flyby algorithm.
        for ch in self.c.populated_channels() {
            self.mch.mc[ch].tc_rwp.modify(TC_RWP::DEC_WRD::SET);
        }
        self.jedec_write_leveling()?;
        for ch in self.c.populated_channels() {
            self.fill_pattern0(ch, 0xaaaa_aaaa, 0x5555_5555);
        }
        for (ch, rank) in self.c.all_ranks() {
            self.tx_dq_write_leveling(ch, rank)?;
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        self.train_write_flyby();
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        Ok(())
    }

    fn test_command_training(&mut self, ch: usize, rank: usize) -> bool {
        let saved = self.c.timings[ch][rank];
        let all = (1u32 << self.c.lanes) - 1;
        let mut lanes_ok = 0u32;
        for (ctr, delta) in (-5..=5).enumerate() {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].tx_dq = saved.lanes[lane].tx_dq + delta;
            }
            self.program_timings(ch);
            for lane in 0..self.c.lanes {
                self.mch.mc[BROADCAST].error_count[lane].set(0);
            }
            // Reset the read WDB pointer.
            self.mch.mc[ch].data_ctl.set(0x1f);
            self.io.wait(ch);
            let c = self.c;
            self.io.command_training(&c, ch, rank as u32, ctr as u32);
            // LFSR seed for the RD/WR sub-sequences.
            self.mch.mc[ch].address_lfsr[1].set(0x389_abcd);
            self.mch.mc[ch].address_lfsr[2].set(0x389_abcd);
            self.io.run_once_and_wait(ch);
            for lane in 0..self.c.lanes {
                if self.mch.mc[ch].error_count[lane].get() == 0 {
                    lanes_ok |= 1 << lane;
                }
            }
            if lanes_ok == all {
                break;
            }
        }
        self.c.timings[ch][rank] = saved;
        lanes_ok != all
    }

    fn reprogram_320c(&mut self) {
        self.disable_refresh_machine();
        self.jedec_reset();
        self.mrs_commands();
        self.toggle_io_reset();
    }

    fn try_cmd_stretch(&mut self, ch: usize, cmd_stretch: u32) -> TrainResult {
        let saved: [RankTimings; NUM_SLOTRANKS] = self.c.timings[ch];
        let mut stats = [[0u32; words(CT_PI_LENGTH)]; NUM_SLOTRANKS];
        self.c.cmd_stretch[ch] = cmd_stretch;
        self.mch.mc[ch].tc_rap.set(self.tc_rap(cmd_stretch));
        let delta = match cmd_stretch {
            2 => 2,
            0 => 4,
            _ => 0,
        };
        for rank in self.c.populated_ranks(ch) {
            self.c.timings[ch][rank].roundtrip_latency -= delta;
        }
        for command_pi in CT_MIN_PI..CT_MAX_PI {
            for rank in self.c.populated_ranks(ch) {
                self.c.timings[ch][rank].pi_coding = command_pi;
            }
            self.program_timings(ch);
            self.reprogram_320c();
            for rank in self.c.populated_ranks(ch) {
                let failed = self.test_command_training(ch, rank);
                set_bit(&mut stats[rank], (command_pi - CT_MIN_PI) as usize, failed);
            }
        }
        for rank in self.c.populated_ranks(ch) {
            let run = longest_zero_run(&stats[rank], CT_PI_LENGTH - 1);
            self.c.timings[ch][rank].pi_coding = run.middle + CT_MIN_PI;
            if run.all || run.length < MIN_C320C_LEN {
                for r in self.c.populated_ranks(ch) {
                    self.c.timings[ch][r] = saved[r];
                }
                return Self::err(ch);
            }
        }
        Ok(())
    }

    /// coreboot `command_training`: adjust the command phase and try the
    /// 1T and 2T command rates.
    pub(super) fn command_training(&mut self) -> TrainResult {
        for ch in self.c.populated_channels() {
            self.fill_pattern5(ch, 0);
        }
        for ch in self.c.populated_channels() {
            // Dual-DIMM channels skip 1T unless an XMP profile asks for it.
            let mut cmdrate = u32::from(self.c.rankmap[ch] & 0x5 == 0x5);
            if self.c.t_cmd != 0 {
                // XMP gives the command rate in clocks, not ns.
                cmdrate = (self.c.t_cmd.div_ceil(256) - 1).min(1);
            }
            let mut result = Self::err(ch);
            while cmdrate < 2 {
                result = self.try_cmd_stretch(ch, cmdrate << 1);
                if result.is_ok() {
                    break;
                }
                cmdrate += 1;
            }
            if result.is_err() {
                fstart_log::error!("sandybridge: command training failed on channel {}", ch);
                return result;
            }
            fstart_log::info!(
                "sandybridge: channel {} uses {}T command rate",
                ch,
                cmdrate + 1
            );
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        self.reprogram_320c();
        Ok(())
    }

    fn find_read_mpr_margin(
        &mut self,
        ch: usize,
        rank: usize,
        edges: &mut [i32; NUM_LANES],
    ) -> TrainResult {
        const LEN: usize = MAX_EDGE_TIMING as usize + 1;
        let mut stats = [[0u32; words(LEN)]; NUM_LANES];
        for dqs_pi in 0..=MAX_EDGE_TIMING {
            for lane in 0..self.c.lanes {
                let l = &mut self.c.timings[ch][rank].lanes[lane];
                l.rx_dqs_p = dqs_pi;
                l.rx_dqs_n = dqs_pi;
            }
            self.program_timings(ch);
            for lane in 0..self.c.lanes {
                self.mch.mc[ch].error_count[lane].set(0);
                let _ = self.mch.mc[ch].bw_serror_c[lane].get();
            }
            self.io.wait(ch);
            let (t_mod, cas) = (self.c.t_mod, self.c.cas);
            self.io.read_mpr(ch, rank as u32, t_mod, 500, 4, 1, cas + 8);
            self.io.run_once_and_wait(ch);
            for lane in 0..self.c.lanes {
                let errors = self.mch.mc[ch].error_count[lane].get();
                set_bit(&mut stats[lane], dqs_pi as usize, errors != 0);
            }
        }
        for lane in 0..self.c.lanes {
            let run = longest_zero_run(&stats[lane], LEN);
            edges[lane] = run.middle;
            if run.all {
                fstart_log::error!(
                    "sandybridge: read MPR training failed {} {} {}",
                    ch,
                    rank,
                    lane
                );
                return Self::err(ch);
            }
        }
        Ok(())
    }

    fn find_predefined_pattern(&mut self, ch: usize) {
        self.fill_pattern0(ch, 0, 0);
        for lane in 0..self.c.lanes {
            self.mch.mc[ch].bw_mask[lane].set(0);
            let _ = self.mch.mc[ch].bw_serror_c[lane].get();
        }
        for code in [16, 48] {
            for rank in self.c.populated_ranks(ch) {
                for lane in 0..self.c.lanes {
                    let l = &mut self.c.timings[ch][rank].lanes[lane];
                    l.rx_dqs_n = code;
                    l.rx_dqs_p = code;
                }
            }
            self.program_timings(ch);
            for rank in self.c.populated_ranks(ch) {
                self.io.wait(ch);
                let (t_mod, cas) = (self.c.t_mod, self.c.cas);
                self.io.read_mpr(ch, rank as u32, t_mod, 3, 4, 1, cas + 8);
                self.io.run_once_and_wait(ch);
            }
        }
        for lane in 0..self.c.lanes {
            let serror = self.mch.mc[ch].bw_serror[lane].get();
            self.mch.mc[ch].bw_mask[lane].set(!serror & 0xff);
        }
    }

    /// coreboot `read_mpr_training`.
    pub(super) fn read_mpr_training(&mut self) -> TrainResult {
        let mut falling = [[[0i32; NUM_LANES]; NUM_SLOTRANKS]; NUM_CHANNELS];
        let mut rising = falling;
        self.mch.training_mod.set(0);
        self.toggle_io_reset();
        for ch in self.c.populated_channels() {
            self.find_predefined_pattern(ch);
            self.fill_pattern0(ch, 0, 0xffff_ffff);
        }
        let dc_mask = &self.mch.mc[BROADCAST].iosav_dc_mask;
        dc_mask.set(3 << 8);
        for (ch, rank) in self.c.all_ranks() {
            self.find_read_mpr_margin(ch, rank, &mut falling[ch][rank])?;
        }
        dc_mask.set(2 << 8);
        for (ch, rank) in self.c.all_ranks() {
            self.find_read_mpr_margin(ch, rank, &mut rising[ch][rank])?;
        }
        dc_mask.set(0);
        for (ch, rank) in self.c.all_ranks() {
            for lane in 0..self.c.lanes {
                let l = &mut self.c.timings[ch][rank].lanes[lane];
                l.rx_dqs_n = falling[ch][rank][lane];
                l.rx_dqs_p = rising[ch][rank][lane];
            }
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        for ch in self.c.populated_channels() {
            for lane in 0..self.c.lanes {
                self.mch.mc[ch].bw_mask[lane].set(0);
            }
        }
        Ok(())
    }

    fn find_agrsv_read_margin(
        &mut self,
        ch: usize,
        rank: usize,
        edges: &mut [i32; NUM_LANES],
    ) -> TrainResult {
        const RD_VREF_OFFSETS: [u32; 3] = [0, 0xc, 0x2c];
        const LEN: usize = MAX_EDGE_TIMING as usize + 1;
        let mut raw = [0u32; LEN];
        let mut lower = [0i32; NUM_LANES];
        let mut upper = [MAX_EDGE_TIMING; NUM_LANES];
        for (i, &vref) in RD_VREF_OFFSETS.iter().enumerate() {
            self.mch.train[ch]
                .training_mod
                .write(TRAINING_MOD::VREF_GEN_CTL.val(vref));
            for pat in 0..NUM_PATTERNS {
                self.fill_pattern5(ch, pat);
                for read_pi in 0..=MAX_EDGE_TIMING {
                    for lane in 0..self.c.lanes {
                        let l = &mut self.c.timings[ch][rank].lanes[lane];
                        l.rx_dqs_p = read_pi;
                        l.rx_dqs_n = read_pi;
                    }
                    self.program_timings(ch);
                    for lane in 0..self.c.lanes {
                        self.mch.mc[ch].error_count[lane].set(0);
                        let _ = self.mch.mc[ch].bw_serror_c[lane].get();
                    }
                    self.io.wait(ch);
                    let c = self.c;
                    self.io.data_write(&c, ch, rank as u32);
                    self.io.run_once_and_wait(ch);
                    for lane in 0..self.c.lanes {
                        let _ = self.mch.mc[ch].error_count[lane].get();
                    }
                    // coreboot reads this Ivy Bridge register on both CPUs.
                    raw[read_pi as usize] = self.mch.mc[ch].byte_serror_c.get();
                }
                for lane in 0..self.c.lanes {
                    let mut stats = [0u32; words(LEN)];
                    for (read_pi, raw) in raw.iter().enumerate() {
                        set_bit(&mut stats, read_pi, raw & (1 << lane) != 0);
                    }
                    let run = longest_zero_run(&stats, LEN);
                    lower[lane] = (run.start + self.c.edge_offset[i]).max(lower[lane]);
                    upper[lane] = (run.end - self.c.edge_offset[i]).min(upper[lane]);
                    edges[lane] = (lower[lane] + upper[lane]) / 2;
                    if run.all || lower[lane] > upper[lane] {
                        fstart_log::error!(
                            "sandybridge: aggressive read training failed {} {} {}",
                            ch,
                            rank,
                            lane
                        );
                        return Self::err(ch);
                    }
                }
            }
        }
        // Restore the nominal Vref.
        self.mch.train[ch].training_mod.set(0);
        Ok(())
    }

    /// coreboot `aggressive_read_training`.
    pub(super) fn aggressive_read_training(&mut self) -> TrainResult {
        let mut falling = [[[0i32; NUM_LANES]; NUM_SLOTRANKS]; NUM_CHANNELS];
        let mut rising = falling;
        let dc_mask = &self.mch.mc[BROADCAST].iosav_dc_mask;
        dc_mask.set(3 << 8);
        for (ch, rank) in self.c.all_ranks() {
            self.find_agrsv_read_margin(ch, rank, &mut falling[ch][rank])?;
        }
        dc_mask.set(2 << 8);
        for (ch, rank) in self.c.all_ranks() {
            self.find_agrsv_read_margin(ch, rank, &mut rising[ch][rank])?;
        }
        dc_mask.set(0);
        for (ch, rank) in self.c.all_ranks() {
            for lane in 0..self.c.lanes {
                let l = &mut self.c.timings[ch][rank].lanes[lane];
                l.rx_dqs_n = falling[ch][rank][lane];
                l.rx_dqs_p = rising[ch][rank][lane];
            }
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        Ok(())
    }

    fn set_write_vref(&self, ch: usize, vref: u32) {
        let mux = &self.mch.cmd_mux[ch].debug_mux;
        mux.set((mux.get() & !(0x3f << 24)) | vref << 24);
        fstart_arch::udelay(2);
    }

    /// coreboot `aggressive_write_training`: write Vref sweep, only on Ivy
    /// Bridge parts that can change it.
    pub(super) fn aggressive_write_training(&mut self) -> TrainResult {
        const WR_VREF_OFFSETS: [u32; 3] = [0, 0x0f, 0x2f];
        const LEN: usize = MAX_TX_DQ as usize + 1;
        let cpu = self.c.cpu();
        if !cpu.is_ivy()
            || !self
                .hb
                .capid0_a
                .is_set(crate::sandybridge::regs::CAPID0_A::WRTVREF)
        {
            return Ok(());
        }
        let mut lower = [[[0i32; NUM_LANES]; NUM_SLOTRANKS]; NUM_CHANNELS];
        let mut upper = [[[MAX_TX_DQ; NUM_LANES]; NUM_SLOTRANKS]; NUM_CHANNELS];
        // IOSAV_n_SP_CMD_ADDR optimization only on later steppings.
        let iosav_opt = cpu.stepping() == 6 || cpu.stepping() >= 8;
        if iosav_opt {
            self.mch.mc[BROADCAST].mcmnts_spare.set(1);
        }
        for (i, &vref) in WR_VREF_OFFSETS.iter().enumerate() {
            for ch in self.c.populated_channels() {
                self.set_write_vref(ch, vref);
                for pat in 0..NUM_PATTERNS {
                    for rank in self.c.populated_ranks(ch) {
                        let mut stats = [0u32; words(LEN)];
                        let mut raw = [0u8; LEN];
                        // Make sure run.start < run.end.
                        set_bit(&mut stats, MAX_TX_DQ as usize, true);
                        self.fill_pattern5(ch, pat);
                        for tx_dq in 0..MAX_TX_DQ {
                            for lane in 0..self.c.lanes {
                                self.c.timings[ch][rank].lanes[lane].tx_dq = tx_dq;
                            }
                            self.program_timings(ch);
                            self.io.wait(ch);
                            let c = self.c;
                            self.io.aggressive_write_read(&c, ch, rank as u32);
                            self.io.run_once_and_wait(ch);
                            raw[tx_dq as usize] = self.mch.mc[ch].byte_serror_c.get() as u8;
                        }
                        for lane in 0..self.c.lanes {
                            for tx_dq in 0..MAX_TX_DQ as usize {
                                set_bit(&mut stats, tx_dq, raw[tx_dq] & (1 << lane) != 0);
                            }
                            let run = longest_zero_run(&stats, LEN);
                            if run.all {
                                fstart_log::error!(
                                    "sandybridge: aggressive write training failed {} {} {}",
                                    ch,
                                    rank,
                                    lane
                                );
                                return Self::err(ch);
                            }
                            let lo = &mut lower[ch][rank][lane];
                            *lo = (run.start + self.c.tx_dq_offset[i]).max(*lo);
                            let hi = &mut upper[ch][rank][lane];
                            *hi = (run.end - self.c.tx_dq_offset[i]).min(*hi);
                        }
                    }
                }
            }
        }
        for ch in 0..NUM_CHANNELS {
            self.set_write_vref(ch, 0);
        }
        if iosav_opt {
            self.mch.mc[BROADCAST].mcmnts_spare.set(0);
        }
        for (ch, rank) in self.c.all_ranks() {
            for lane in 0..self.c.lanes {
                self.c.timings[ch][rank].lanes[lane].tx_dq =
                    (lower[ch][rank][lane] + upper[ch][rank][lane]) / 2;
            }
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
        Ok(())
    }

    /// coreboot `normalize_training`.
    pub(super) fn normalize_training(&mut self) {
        for (ch, rank) in self.c.all_ranks() {
            let lanes = self.c.lanes;
            let t = self.c.rank(ch, rank);
            let mat = t.lanes[..lanes].iter().map(|l| l.rcven).fold(0, i32::max);
            let delta = (mat >> 6) - t.io_latency;
            t.roundtrip_latency += delta;
            t.io_latency += delta;
        }
        for ch in self.c.populated_channels() {
            self.program_timings(ch);
        }
    }

    /// coreboot `channel_test`: a short memory test per rank.
    pub(super) fn channel_test(&mut self) -> TrainResult {
        for ch in self.c.populated_channels() {
            if self.mch.mc[ch].mc_init_state.get() & 0xa000 != 0 {
                fstart_log::error!("sandybridge: mini channel test failed (1): {}", ch);
                return Self::err(ch);
            }
        }
        for ch in self.c.populated_channels() {
            self.fill_pattern0(ch, 0x1234_5678, 0x9876_5432);
        }
        for rank in 0..NUM_SLOTRANKS {
            for ch in 0..NUM_CHANNELS {
                if self.c.rankmap[ch] & (1 << rank) == 0 {
                    continue;
                }
                for lane in 0..self.c.lanes {
                    self.mch.mc[BROADCAST].error_count[lane].set(0);
                    self.mch.mc[BROADCAST].bw_serror_c[lane].set(0);
                }
                self.io.wait(ch);
                self.io.memory_test(ch, rank as u32);
                self.io.run_once_and_wait(ch);
                for lane in 0..self.c.lanes {
                    if self.mch.mc[ch].error_count[lane].get() != 0 {
                        fstart_log::error!(
                            "sandybridge: mini channel test failed (2): {} {} {}",
                            ch,
                            rank,
                            lane
                        );
                        return Self::err(ch);
                    }
                }
            }
        }
        Ok(())
    }

    /// coreboot `channel_scrub`: write every DRAM location once so the ECC
    /// bits are valid.
    pub(super) fn channel_scrub(&mut self) {
        for ch in self.c.populated_channels() {
            self.io.wait(ch);
            self.fill_pattern0(ch, 0, 0);
        }
        for (ch, rank) in self.c.all_ranks() {
            let rowsize = 1u32 << self.c.dimms[ch][rank >> 1].row_bits;
            let c = self.c;
            let gap = ((c.t_faw >> 2) + 1).max(c.t_rrd);
            for bank in 0..8 {
                for row in (0..rowsize).step_by(16) {
                    let r = rank as u32;
                    self.io.write(
                        ch,
                        &[
                            iosav::Ssq {
                                gap,
                                post: c.t_rcd,
                                address: row,
                                bank,
                                update: iosav::Update {
                                    inc_addr_1: true,
                                    ..iosav::Update::wrap(18)
                                },
                                ..iosav::Ssq::new(iosav::ACT, r)
                            },
                            iosav::Ssq {
                                executions: 129,
                                gap: 4,
                                post: c.t_wtr + c.cwl + 8,
                                dir: iosav::Dir::Write,
                                address: row,
                                rowbits: 0,
                                bank,
                                update: iosav::Update::addr8(9),
                                ..iosav::Ssq::new(iosav::WR, r)
                            },
                            iosav::Ssq {
                                gap: 4,
                                post: c.t_rp,
                                bank,
                                update: iosav::Update::wrap(18),
                                ..iosav::Ssq::new(iosav::PRE, r)
                            },
                        ],
                    );
                    self.io.run(ch, 16, false);
                    self.io.wait(ch);
                }
            }
        }
    }
}

/// coreboot `get_dqs_flyby_adjust`.
fn dqs_flyby_adjust(val: u64) -> i32 {
    if val == u64::MAX {
        return 0;
    }
    if val >= 0xf000_0000_0000_0000 {
        // DQS is late: negative adjustment.
        for i in 0..8 {
            if val << (8 * (7 - i) + 4) != 0 {
                return -i;
            }
        }
    } else {
        // DQS is early: positive adjustment.
        for i in 0..8 {
            if val >> (8 * (7 - i) + 4) != 0 {
                return i;
            }
        }
    }
    8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_zero_run_wraps_and_reports_all_zero() {
        let all_zero = [0u32; 4];
        let run = longest_zero_run(&all_zero, 128);
        assert!(run.all);
        assert_eq!(run.middle, 64);
        // Failures at 10 and 20: the longest pass window wraps from 21 to 9.
        let mut seq = [0u32; 4];
        set_bit(&mut seq, 10, true);
        set_bit(&mut seq, 20, true);
        let run = longest_zero_run(&seq, 128);
        assert!(!run.all);
        assert_eq!(run.start, 21);
        assert_eq!(run.end, 9);
        assert_eq!(run.length, 117);
        assert_eq!(run.middle, 21 + 58);
    }

    #[test]
    fn flyby_adjust_matches_coreboot() {
        assert_eq!(dqs_flyby_adjust(u64::MAX), 0);
        assert_eq!(dqs_flyby_adjust(0), 8);
        // Early: the highest set byte above the low nibble.
        assert_eq!(dqs_flyby_adjust(0x10), 7);
        assert_eq!(dqs_flyby_adjust(1 << 60), 0);
        // Late: nothing below the top nibble means "past every lane".
        assert_eq!(dqs_flyby_adjust(0xf000_0000_0000_0000), 8);
        assert_eq!(dqs_flyby_adjust(0xf000_0000_0000_0010), -1);
    }
}
