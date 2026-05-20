//! Write-leveling pieces of Sandy Bridge native raminit.

use core::sync::atomic::{compiler_fence, Ordering};

use fstart_services::ServiceError;

use super::iosav;
use super::mchbar;
use super::mrs;
use super::training::{MAX_TX_DQ, NUM_LANES, QCLK_PI};
use super::RaminitState;

const LANE_BASES: [usize; NUM_LANES] = [
    0x0000, 0x0200, 0x0400, 0x0600, 0x1000, 0x1200, 0x1400, 0x1600, 0x0800,
];

const GDCRTRAININGRESULT_BASE: usize = 0x0004;
const IOSAV_BY_BW_SERROR_C_CH_BASE: usize = 0x4140;
const IOSAV_BY_ERROR_COUNT_CH_BASE: usize = 0x4340;
const IOSAV_DATA_CTL_CH_BASE: usize = 0x4288;
const GDCRTRAININGMOD: usize = 0x3400;
const TC_RWP_BASE: usize = 0x4008;
const SCHED_CBIT_CH_BASE: usize = 0x4020;
const MC_INIT_STATE_G: usize = 0x5030;
const WDB_BASE: usize = 0x0400_0000;
const TX_DQS_PI_LENGTH: usize = 2 * (QCLK_PI as usize);

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn cxly(base: usize, channel: usize, lane: usize) -> usize {
    base + (channel << 10) + (lane << 2)
}

#[inline]
const fn gzly(base: usize, channel: usize, index: usize) -> usize {
    base + (channel << 8) + (index << 2)
}

/// Run the ported Sandy Bridge write-training stages.
pub fn train(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            // DEC_WRD is required for the write fly-by algorithm and coreboot
            // sets it before all write-training stages.
            mchbar::setbits32(mchbar_base + cx(TC_RWP_BASE, channel), 1 << 27);
        }
    }

    jedec_write_leveling(mchbar_base, state)?;

    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            fill_pattern0(mchbar_base, state, channel, 0xaaaa_aaaa, 0x5555_5555);
        }
    }

    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    for (channel, slotrank) in ranks {
        tx_dq_write_leveling(mchbar_base, state, channel, slotrank)?;
    }
    mchbar::program_rank_timings(mchbar_base, &state.training);
    train_write_flyby(mchbar_base, state)?;
    mchbar::program_rank_timings(mchbar_base, &state.training);

    Ok(())
}

/// Coreboot calls aggressive write training after aggressive read training, but
/// the Sandy Bridge implementation returns immediately (only Ivy Bridge uses
/// the body). Keep an explicit no-op stage so the top-level sequence mirrors
/// coreboot without claiming Ivy-specific support.
pub fn aggressive_train(
    _mchbar_base: usize,
    _state: &mut RaminitState,
) -> Result<(), ServiceError> {
    Ok(())
}

fn jedec_write_leveling(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    disable_refresh_machine(mchbar_base, state)?;
    set_all_ranks_write_leveling(mchbar_base, state, true)?;
    mchbar::write32(
        mchbar_base + GDCRTRAININGMOD,
        training_mod_write_leveling_common(),
    );
    mchbar::toggle_io_reset(mchbar_base);

    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    for (channel, slotrank) in ranks {
        write_level_rank(mchbar_base, state, channel, slotrank)?;
    }

    set_all_ranks_write_leveling(mchbar_base, state, false)?;
    mchbar::write32(mchbar_base + GDCRTRAININGMOD, 0);
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            iosav::wait(mchbar_base, channel)?;
        }
    }
    enable_refresh_machine(mchbar_base, state)?;
    mchbar::toggle_io_reset(mchbar_base);
    mchbar::program_rank_timings(mchbar_base, &state.training);
    Ok(())
}

fn set_all_ranks_write_leveling(
    mchbar_base: usize,
    state: &RaminitState,
    enable: bool,
) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        for slotrank in 0..4 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
                continue;
            }
            let mut mr1 = state.mode_registers[channel][slotrank].mr1;
            if enable {
                mr1 |= (1 << 7) | (1 << 12);
            }
            write_mrreg(mchbar_base, state, channel, slotrank, 1, mr1)?;
        }
    }
    Ok(())
}

fn write_mrreg(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
    mut bank: u8,
    mut value: u16,
) -> Result<(), ServiceError> {
    if state.topology.rank_mirror[channel][slotrank] {
        (bank, value) = mrs::mirror_mr_address(bank, value);
    }

    let mut first = iosav::SequenceStep::simple(iosav::IOSAV_MRS, slotrank as u8, bank, value, 4);
    first.cmd_delay_gap = 4;

    let mut second = first;
    second.ranksel_ap = 1;

    let mut third = first;
    third.post_ssq_wait = state.timing.tmod as u16;

    iosav::wait(mchbar_base, channel)?;
    iosav::write_sequence(mchbar_base, channel, &[first, second, third]);
    iosav::run_once_and_wait(mchbar_base, channel, 3)
}

fn disable_refresh_machine(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        let slotrank = if (state.topology.rankmap[channel] & 1) == 0 {
            2
        } else {
            0
        };
        iosav::write_zqcs_sequence(mchbar_base, channel, slotrank, 4, 4, 31);
        iosav::run_once_and_wait(mchbar_base, channel, 1)?;
        mchbar::setbits32(mchbar_base + cx(SCHED_CBIT_CH_BASE, channel), 1 << 21);
    }
    mchbar::clrbits32(mchbar_base + MC_INIT_STATE_G, 1 << 3);
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            iosav::run_once_and_wait(mchbar_base, channel, 1)?;
        }
    }
    Ok(())
}

fn enable_refresh_machine(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    mchbar::setbits32(mchbar_base + MC_INIT_STATE_G, 1 << 3);
    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        mchbar::clrbits32(mchbar_base + cx(SCHED_CBIT_CH_BASE, channel), 1 << 21);
        let _ = iosav::read_status(mchbar_base, channel);
        let slotrank = if (state.topology.rankmap[channel] & 1) == 0 {
            2
        } else {
            0
        };
        iosav::wait(mchbar_base, channel)?;
        iosav::write_zqcs_sequence(mchbar_base, channel, slotrank, 4, 101, 31);
        iosav::run_once_and_wait(mchbar_base, channel, 1)?;
    }
    Ok(())
}

fn write_level_rank(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<(), ServiceError> {
    mchbar::write32(
        mchbar_base + GDCRTRAININGMOD,
        training_mod_write_leveling(slotrank as u8),
    );

    let mut mr1 = state.mode_registers[channel][slotrank].mr1 | (1 << 7);
    let mut bank = 1u8;
    if state.topology.rank_mirror[channel][slotrank] {
        (bank, mr1) = mrs::mirror_mr_address(bank, mr1);
    }

    iosav::wait(mchbar_base, channel)?;
    iosav::write_jedec_write_leveling_sequence(
        mchbar_base,
        channel,
        slotrank as u8,
        bank,
        mr1,
        u16::from(state.timing.cwl),
        state.timing.twlo as u16,
        u16::from(state.timing.cas),
        state.timing.tmod as u16,
    );

    let mut statistics = [[0u8; TX_DQS_PI_LENGTH]; NUM_LANES];
    for tx_dqs in 0..TX_DQS_PI_LENGTH {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].tx_dqs = tx_dqs as u16;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        iosav::run_once_and_wait(mchbar_base, channel, 4)?;
        for (lane, stats) in statistics
            .iter_mut()
            .enumerate()
            .take(state.training.active_lanes)
        {
            let result = mchbar::read32(
                mchbar_base
                    + LANE_BASES[lane]
                    + gzly(GDCRTRAININGRESULT_BASE, channel, (tx_dqs / 32) & 1),
            );
            stats[tx_dqs] = u8::from(((result >> (tx_dqs % 32)) & 1) == 0);
        }
    }

    for (lane, stats) in statistics
        .iter()
        .enumerate()
        .take(state.training.active_lanes)
    {
        let mut run = longest_zero_run(stats);
        if (run.start & 0x3f) == 0x3e {
            run.start += 2;
        } else if (run.start & 0x3f) == 0x3f {
            run.start += 1;
        }
        if run.all {
            return Err(ServiceError::HardwareError);
        }
        state.training.ranks[channel][slotrank].lanes[lane].tx_dqs = run.start as u16;
    }
    Ok(())
}

fn tx_dq_write_leveling(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<(), ServiceError> {
    iosav::wait(mchbar_base, channel)?;
    iosav::write_prea_sequence(
        mchbar_base,
        channel,
        slotrank as u8,
        state.timing.trp as u16,
        18,
    );
    iosav::run_once_and_wait(mchbar_base, channel, 1)?;

    let mut stats = [[0u32; (MAX_TX_DQ as usize) + 1]; NUM_LANES];
    for tx_dq in 0..=MAX_TX_DQ {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].tx_dq = tx_dq;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        test_tx_dq(mchbar_base, state, channel, slotrank)?;
        for (lane, lane_stats) in stats
            .iter_mut()
            .enumerate()
            .take(state.training.active_lanes)
        {
            lane_stats[tx_dq as usize] =
                mchbar::read32(mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane));
        }
    }

    for (lane, lane_stats) in stats
        .iter_mut()
        .enumerate()
        .take(state.training.active_lanes)
    {
        let mut run = longest_zero_run_u32(lane_stats);
        if run.all || run.length < 8 {
            threshold_process(lane_stats);
            run = longest_zero_run_u32(lane_stats);
            if run.all || run.length < 8 {
                return Err(ServiceError::HardwareError);
            }
        }
        state.training.ranks[channel][slotrank].lanes[lane].tx_dq = run.middle as i16;
    }
    Ok(())
}

fn test_tx_dq(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<(), ServiceError> {
    for lane in 0..state.training.active_lanes {
        mchbar::write32(
            mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane),
            0,
        );
        let _ = mchbar::read32(mchbar_base + cxly(IOSAV_BY_BW_SERROR_C_CH_BASE, channel, lane));
    }

    iosav::wait(mchbar_base, channel)?;
    iosav::write_misc_write_sequence(
        mchbar_base,
        channel,
        slotrank as u8,
        iosav::MiscWriteTiming {
            trcd: state.timing.trcd as u16,
            cwl: u16::from(state.timing.cwl),
            twtr: state.timing.twtr as u16,
        },
        state.timing.trrd.max((state.timing.tfaw >> 2) + 1) as u8,
        4,
        4,
        500,
        18,
    );
    iosav::run_once_and_wait(mchbar_base, channel, 4)?;

    iosav::write_prea_act_read_sequence(
        mchbar_base,
        channel,
        slotrank as u8,
        iosav::PreaActReadTiming {
            trp: state.timing.trp as u16,
            trrd: state.timing.trrd as u8,
            tfaw: state.timing.tfaw as u8,
            cas: u16::from(state.timing.cas),
            trtp: state.timing.trtp as u16,
        },
    );
    iosav::run_once_and_wait(mchbar_base, channel, 4)
}

fn fill_pattern0(mchbar_base: usize, state: &RaminitState, channel: usize, a: u32, b: u32) {
    let channel_offset = preceding_channels(state, channel) * 64;
    for j in 0..16usize {
        let val = if (j & 2) != 0 { b } else { a };
        write_pattern32(WDB_BASE + channel_offset + 4 * j, val);
    }
    compiler_fence(Ordering::SeqCst);
    mchbar::write8(mchbar_base + cx(IOSAV_DATA_CTL_CH_BASE, channel), 0);
}

fn preceding_channels(state: &RaminitState, target_channel: usize) -> usize {
    (0..target_channel)
        .filter(|channel| state.topology.rankmap[*channel] != 0)
        .count()
}

fn write_pattern32(addr: usize, val: u32) {
    // SAFETY: coreboot uses this fixed WDB staging aperture during native raminit.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

fn threshold_process(data: &mut [u32]) {
    let mut min = data[0];
    let mut max = data[0];
    for val in data.iter().copied().skip(1) {
        min = min.min(val);
        max = max.max(val);
    }
    let threshold = min / 2 + max / 2;
    for val in data.iter_mut() {
        *val = u32::from(*val > threshold);
    }
}

fn train_write_flyby(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    mchbar::write32(mchbar_base + GDCRTRAININGMOD, 1 << 9);
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            fill_pattern1(mchbar_base, state, channel);
        }
    }

    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    for (channel, slotrank) in ranks {
        mchbar::write32(
            mchbar_base + cx(IOSAV_DATA_CTL_CH_BASE, channel),
            0x0001_0001,
        );
        iosav::wait(mchbar_base, channel)?;
        iosav::write_misc_write_sequence(
            mchbar_base,
            channel,
            slotrank as u8,
            iosav::MiscWriteTiming {
                trcd: state.timing.trcd as u16,
                cwl: u16::from(state.timing.cwl),
                twtr: state.timing.twtr as u16,
            },
            3,
            1,
            3,
            3,
            31,
        );
        iosav::run_once_and_wait(mchbar_base, channel, 4)?;

        write_flyby_read_sequence(mchbar_base, state, channel, slotrank);
        iosav::run_once_and_wait(mchbar_base, channel, 3)?;

        for lane in 0..state.training.active_lanes {
            let low = mchbar::read32(
                mchbar_base + LANE_BASES[lane] + gzly(GDCRTRAININGRESULT_BASE, channel, 0),
            ) as u64;
            let high = mchbar::read32(
                mchbar_base + LANE_BASES[lane] + gzly(GDCRTRAININGRESULT_BASE, channel, 1),
            ) as u64;
            let adjust = get_dqs_flyby_adjust(low | (high << 32));
            let lane_timing = &mut state.training.ranks[channel][slotrank].lanes[lane];
            lane_timing.tx_dqs =
                ((i32::from(lane_timing.tx_dqs)) + adjust * i32::from(QCLK_PI)).max(0) as u16;
        }
    }
    mchbar::write32(mchbar_base + GDCRTRAININGMOD, 0);
    Ok(())
}

fn write_flyby_read_sequence(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
) {
    let mut prea = iosav::SequenceStep::simple(
        iosav::IOSAV_PRE,
        slotrank as u8,
        0,
        1 << 10,
        state.timing.trp as u16,
    );
    prea.ranksel_ap = 1;
    prea.cmd_delay_gap = 3;
    prea.addr_wrap = 18;

    let mut act = iosav::SequenceStep::simple(
        iosav::IOSAV_ACT,
        slotrank as u8,
        0,
        0,
        state.timing.trcd as u16,
    );
    act.ranksel_ap = 1;
    act.cmd_delay_gap = 3;

    let post = state.timing.trp as u16
        + u16::from(state.training.ranks[channel][slotrank].roundtrip_latency)
        + u16::from(state.training.ranks[channel][slotrank].io_latency);
    let mut read = iosav::SequenceStep::simple(iosav::IOSAV_RD, slotrank as u8, 0, 8, post);
    read.ranksel_ap = 3;
    read.cmd_delay_gap = 3;
    read.data_direction = iosav::SSQ_RD;

    iosav::write_sequence(mchbar_base, channel, &[prea, act, read]);
}

fn get_dqs_flyby_adjust(val: u64) -> i32 {
    if val == u64::MAX {
        return 0;
    }
    if val >= 0xf000_0000_0000_0000 {
        for i in 0..8 {
            if (val << (8 * (7 - i) + 4)) != 0 {
                return -i;
            }
        }
    } else {
        for i in 0..8 {
            if (val >> (8 * (7 - i) + 4)) != 0 {
                return i;
            }
        }
    }
    8
}

fn fill_pattern1(mchbar_base: usize, state: &RaminitState, channel: usize) {
    let channel_offset = preceding_channels(state, channel) * 64;
    let channel_step = 64 * num_channels(state);
    for j in 0..16usize {
        write_pattern32(WDB_BASE + channel_offset + j * 4, 0xffff_ffff);
    }
    for j in 0..16usize {
        write_pattern32(WDB_BASE + channel_offset + channel_step + j * 4, 0);
    }
    compiler_fence(Ordering::SeqCst);
    mchbar::write8(mchbar_base + cx(IOSAV_DATA_CTL_CH_BASE, channel), 1);
}

fn num_channels(state: &RaminitState) -> usize {
    (0..2)
        .filter(|channel| state.topology.rankmap[*channel] != 0)
        .count()
}

fn training_mod_write_leveling_common() -> u32 {
    (1 << 1) | (5 << 4) | (1 << 15) | (1 << 20)
}

fn training_mod_write_leveling(slotrank: u8) -> u32 {
    training_mod_write_leveling_common() | ((u32::from(slotrank) & 0x3) << 2)
}

#[derive(Clone, Copy, Default)]
struct Run {
    start: usize,
    middle: usize,
    length: usize,
    all: bool,
}

fn longest_zero_run(seq: &[u8]) -> Run {
    let sz = seq.len();
    let mut best_len = 0usize;
    let mut best_start = 0usize;
    let mut last_start = 0usize;
    for i in 0..(2 * sz) {
        if seq[i % sz] != 0 {
            if i - last_start > best_len {
                best_len = i - last_start;
                best_start = last_start;
            }
            last_start = i + 1;
        }
    }
    if best_len == 0 {
        return Run {
            start: 0,
            middle: sz / 2,
            length: sz,
            all: true,
        };
    }
    Run {
        start: best_start % sz,
        middle: (best_start + (best_len - 1) / 2) % sz,
        length: best_len,
        all: false,
    }
}

fn longest_zero_run_u32(seq: &[u32]) -> Run {
    let sz = seq.len();
    let mut best_len = 0usize;
    let mut best_start = 0usize;
    let mut last_start = 0usize;
    for i in 0..(2 * sz) {
        if seq[i % sz] != 0 {
            if i - last_start > best_len {
                best_len = i - last_start;
                best_start = last_start;
            }
            last_start = i + 1;
        }
    }
    if best_len == 0 {
        return Run {
            start: 0,
            middle: sz / 2,
            length: sz,
            all: true,
        };
    }
    Run {
        start: best_start % sz,
        middle: (best_start + (best_len - 1) / 2) % sz,
        length: best_len,
        all: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_write_leveling_training_mode() {
        assert_eq!(
            training_mod_write_leveling(3),
            (1 << 1) | (3 << 2) | (5 << 4) | (1 << 15) | (1 << 20)
        );
    }

    #[test]
    fn write_leveling_window_uses_first_zero() {
        let seq = [1, 1, 0, 0, 0, 1];
        let run = longest_zero_run(&seq);
        assert_eq!(run.start, 2);
        assert!(!run.all);
    }
}
