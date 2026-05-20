//! Receive-enable calibration for Sandy Bridge native raminit.

use fstart_arch_x86::udelay;
use fstart_services::ServiceError;

use super::iosav;
use super::mchbar;
use super::training::{NUM_LANES, QCLK_PI};
use super::RaminitState;

const LANE_BASES: [usize; NUM_LANES] = [
    0x0000, 0x0200, 0x0400, 0x0600, 0x1000, 0x1200, 0x1400, 0x1600, 0x0800,
];

const GDCRTRAININGRESULT_BASE: usize = 0x0004;
const GDCRTRAININGMOD: usize = 0x3400;
const MC_INIT_STATE_G: usize = 0x5030;

const RCVEN_COARSE_PI_LENGTH: usize = 2 * (QCLK_PI as usize);

#[inline]
const fn gzly(base: usize, channel: usize, index: usize) -> usize {
    base + (channel << 8) + (index << 2)
}

/// Port of coreboot `receive_enable_calibration()`.
pub fn calibrate(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    for (channel, slotrank) in ranks {
        let mut upper = [0u16; NUM_LANES];

        iosav::wait(mchbar_base, channel)?;
        iosav::write_prea_sequence(
            mchbar_base,
            channel,
            slotrank as u8,
            state.timing.trp as u16,
            0,
        );
        iosav::run_once_and_wait(mchbar_base, channel, 1)?;

        mchbar::write32(
            mchbar_base + GDCRTRAININGMOD,
            training_mod_receive_enable(slotrank as u8),
        );

        state.training.ranks[channel][slotrank].io_latency = 4;
        state.training.ranks[channel][slotrank].roundtrip_latency = 55;
        mchbar::program_rank_timings(mchbar_base, &state.training);

        find_rcven_pi_coarse(mchbar_base, state, channel, slotrank, &mut upper)?;

        let mut all_high = true;
        let mut some_high = false;
        for lane in 0..state.training.active_lanes {
            if state.training.ranks[channel][slotrank].lanes[lane].rcven >= QCLK_PI as u16 {
                some_high = true;
            } else {
                all_high = false;
            }
        }

        if all_high {
            state.training.ranks[channel][slotrank].io_latency = state.training.ranks[channel]
                [slotrank]
                .io_latency
                .saturating_sub(1);
            for (lane, upper_lane) in upper
                .iter_mut()
                .enumerate()
                .take(state.training.active_lanes)
            {
                state.training.ranks[channel][slotrank].lanes[lane].rcven -= QCLK_PI as u16;
                *upper_lane -= QCLK_PI as u16;
            }
        } else if some_high {
            state.training.ranks[channel][slotrank].roundtrip_latency += 1;
            state.training.ranks[channel][slotrank].io_latency += 1;
        }

        mchbar::program_rank_timings(mchbar_base, &state.training);
        let mut prev = logic_delay_delta(state, channel, slotrank)?;
        find_roundtrip_latency(mchbar_base, state, channel, slotrank, &mut upper)?;
        prev = align_rt_io_latency(state, channel, slotrank, prev);
        fine_tune_rcven_pi(mchbar_base, state, channel, slotrank, &mut upper)?;
        prev = align_rt_io_latency(state, channel, slotrank, prev);
        compute_final_logic_delay(state, channel, slotrank)?;
        let _ = align_rt_io_latency(state, channel, slotrank, prev);

        mchbar::write32(mchbar_base + GDCRTRAININGMOD, 0);
        toggle_io_reset(mchbar_base);
    }

    mchbar::program_rank_timings(mchbar_base, &state.training);
    Ok(())
}

fn training_mod_receive_enable(slotrank: u8) -> u32 {
    1 | ((u32::from(slotrank) & 0x3) << 2) | (1 << 15)
}

fn test_rcven(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<(), ServiceError> {
    iosav::wait(mchbar_base, channel)?;
    iosav::write_read_mpr_sequence(
        mchbar_base,
        channel,
        slotrank as u8,
        state.timing.tmod as u16,
        1,
        3,
        15,
        u16::from(state.timing.cas) + 36,
    );
    iosav::run_once_and_wait(mchbar_base, channel, 4)
}

fn does_lane_work(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
    lane: usize,
) -> bool {
    let rcven = state.training.ranks[channel][slotrank].lanes[lane].rcven;
    let reg = mchbar::read32(
        mchbar_base
            + LANE_BASES[lane]
            + gzly(
                GDCRTRAININGRESULT_BASE,
                channel,
                ((rcven / 32) & 1) as usize,
            ),
    );
    ((reg >> (rcven % 32)) & 1) != 0
}

#[derive(Clone, Copy, Default)]
struct Run {
    middle: usize,
    end: usize,
    start: usize,
    all: bool,
    length: usize,
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
            middle: sz / 2,
            start: 0,
            end: sz,
            length: sz,
            all: true,
        };
    }
    Run {
        start: best_start % sz,
        end: (best_start + best_len - 1) % sz,
        middle: (best_start + (best_len - 1) / 2) % sz,
        length: best_len,
        all: false,
    }
}

fn find_rcven_pi_coarse(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    upper: &mut [u16; NUM_LANES],
) -> Result<(), ServiceError> {
    let mut statistics = [[0u8; RCVEN_COARSE_PI_LENGTH]; NUM_LANES];
    for rcven in 0..RCVEN_COARSE_PI_LENGTH {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].rcven = rcven as u16;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        test_rcven(mchbar_base, state, channel, slotrank)?;
        for (lane, stats) in statistics
            .iter_mut()
            .enumerate()
            .take(state.training.active_lanes)
        {
            stats[rcven] = u8::from(!does_lane_work(mchbar_base, state, channel, slotrank, lane));
        }
    }

    for (lane, stats) in statistics
        .iter()
        .enumerate()
        .take(state.training.active_lanes)
    {
        let run = longest_zero_run(stats);
        state.training.ranks[channel][slotrank].lanes[lane].rcven = run.middle as u16;
        upper[lane] = run.end as u16;
        if upper[lane] < run.middle as u16 {
            upper[lane] += (2 * QCLK_PI) as u16;
        }
    }
    Ok(())
}

fn fine_tune_rcven_pi(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    upper: &mut [u16; NUM_LANES],
) -> Result<(), ServiceError> {
    let mut statistics = [[0u8; 51]; NUM_LANES];
    for delta in -25i16..=25 {
        for (lane, upper_lane) in upper.iter().enumerate().take(state.training.active_lanes) {
            state.training.ranks[channel][slotrank].lanes[lane].rcven =
                ((*upper_lane as i16) + delta + QCLK_PI) as u16;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        for _ in 0..100 {
            test_rcven(mchbar_base, state, channel, slotrank)?;
            for (lane, stats) in statistics
                .iter_mut()
                .enumerate()
                .take(state.training.active_lanes)
            {
                stats[(delta + 25) as usize] +=
                    u8::from(does_lane_work(mchbar_base, state, channel, slotrank, lane));
            }
        }
    }

    for (lane, stats) in statistics
        .iter()
        .enumerate()
        .take(state.training.active_lanes)
    {
        let mut last_zero = -26i16;
        for delta in -25i16..=25 {
            if stats[(delta + 25) as usize] != 0 {
                break;
            }
            last_zero = delta;
        }
        let mut first_all = 26i16;
        for delta in -25i16..=25 {
            if stats[(delta + 25) as usize] == 100 {
                first_all = delta;
                break;
            }
        }
        state.training.ranks[channel][slotrank].lanes[lane].rcven =
            (((last_zero + first_all) / 2) + upper[lane] as i16) as u16;
    }
    Ok(())
}

fn find_roundtrip_latency(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    upper: &mut [u16; NUM_LANES],
) -> Result<(), ServiceError> {
    loop {
        mchbar::program_rank_timings(mchbar_base, &state.training);
        test_rcven(mchbar_base, state, channel, slotrank)?;

        let mut all_work = true;
        let mut some_work = false;
        let mut works = [false; NUM_LANES];
        for (lane, work) in works
            .iter_mut()
            .enumerate()
            .take(state.training.active_lanes)
        {
            *work = !does_lane_work(mchbar_base, state, channel, slotrank, lane);
            if *work {
                some_work = true;
            } else {
                all_work = false;
            }
        }

        if all_work {
            return Ok(());
        }
        if !some_work {
            let rank = &mut state.training.ranks[channel][slotrank];
            if rank.roundtrip_latency < 2 {
                return Err(ServiceError::HardwareError);
            }
            rank.roundtrip_latency -= 2;
            continue;
        }

        let rank = &mut state.training.ranks[channel][slotrank];
        rank.io_latency += 2;
        if rank.io_latency >= 16 {
            return Err(ServiceError::HardwareError);
        }
        for (lane, work) in works.iter().enumerate().take(state.training.active_lanes) {
            if *work {
                rank.lanes[lane].rcven += (2 * QCLK_PI) as u16;
                upper[lane] += (2 * QCLK_PI) as u16;
            }
        }
    }
}

fn logic_delay_delta(
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<i16, ServiceError> {
    let mut min_delay = 7u16;
    let mut max_delay = 0u16;
    for lane in 0..state.training.active_lanes {
        let delay = state.training.ranks[channel][slotrank].lanes[lane].rcven >> 6;
        min_delay = min_delay.min(delay);
        max_delay = max_delay.max(delay);
    }
    if max_delay < min_delay {
        return Err(ServiceError::HardwareError);
    }
    Ok((max_delay - min_delay) as i16)
}

fn align_rt_io_latency(
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    prev: i16,
) -> i16 {
    let post = logic_delay_delta(state, channel, slotrank).unwrap_or(prev);
    let latency_offset = if prev < post {
        1
    } else if prev > post {
        -1
    } else {
        0
    };
    let rank = &mut state.training.ranks[channel][slotrank];
    rank.io_latency = ((i16::from(rank.io_latency)) + latency_offset).max(0) as u8;
    rank.roundtrip_latency = ((i16::from(rank.roundtrip_latency)) + latency_offset).max(0) as u8;
    post
}

fn compute_final_logic_delay(
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<(), ServiceError> {
    let mut min_delay = 7u16;
    for lane in 0..state.training.active_lanes {
        let delay = state.training.ranks[channel][slotrank].lanes[lane].rcven >> 6;
        min_delay = min_delay.min(delay);
    }
    let rank = &mut state.training.ranks[channel][slotrank];
    for lane in 0..state.training.active_lanes {
        rank.lanes[lane].rcven -= min_delay << 6;
    }
    rank.io_latency = rank
        .io_latency
        .checked_sub(min_delay as u8)
        .ok_or(ServiceError::HardwareError)?;
    Ok(())
}

fn toggle_io_reset(mchbar_base: usize) {
    let val = mchbar::read32(mchbar_base + MC_INIT_STATE_G);
    mchbar::write32(mchbar_base + MC_INIT_STATE_G, val | (1 << 5));
    udelay(1);
    mchbar::write32(mchbar_base + MC_INIT_STATE_G, val & !(1 << 5));
    udelay(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_wrapping_zero_run_middle() {
        let seq = [0, 0, 1, 1, 0, 0, 0, 1];
        let run = longest_zero_run(&seq);
        assert_eq!(run.start, 4);
        assert_eq!(run.middle, 5);
        assert_eq!(run.end, 6);
        assert_eq!(run.length, 3);
        assert!(!run.all);
    }

    #[test]
    fn packs_receive_enable_training_mode() {
        assert_eq!(training_mod_receive_enable(2), 1 | (2 << 2) | (1 << 15));
    }
}
