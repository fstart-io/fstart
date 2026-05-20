//! Read MPR training for Sandy Bridge native raminit.

use fstart_services::ServiceError;

use super::iosav;
use super::mchbar;
use super::patterns;
use super::training::{MAX_EDGE_TIMING, NUM_LANES};
use super::RaminitState;

const IOSAV_BY_BW_SERROR_CH_BASE: usize = 0x4040;
const IOSAV_BY_BW_MASK_CH_BASE: usize = 0x4080;
const IOSAV_BY_BW_SERROR_C_CH_BASE: usize = 0x4140;
const IOSAV_BY_ERROR_COUNT_CH_BASE: usize = 0x4340;
const IOSAV_BYTE_SERROR_C_CH_BASE: usize = 0x436c;
const IOSAV_DC_MASK: usize = 0x4eb0;
const GDCRTRAININGMOD_CH_BASE: usize = 0x3000;
const GDCRTRAININGMOD: usize = 0x3400;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn cxly(base: usize, channel: usize, lane: usize) -> usize {
    base + (channel << 10) + (lane << 2)
}

#[inline]
const fn gz(base: usize, channel: usize) -> usize {
    base + (channel << 8)
}

/// Port of coreboot `read_mpr_training()`.
pub fn train(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    let channels: heapless::Vec<_, 2> = (0..2)
        .filter(|channel| state.topology.rankmap[*channel] != 0)
        .collect();

    mchbar::write32(mchbar_base + GDCRTRAININGMOD, 0);
    mchbar::toggle_io_reset(mchbar_base);
    for channel in channels.iter().copied() {
        find_predefined_pattern(mchbar_base, state, channel)?;
        patterns::fill_pattern0(mchbar_base, state, channel, 0, 0xffff_ffff);
    }

    let mut falling_edges = [[[0u8; NUM_LANES]; 4]; 2];
    let mut rising_edges = [[[0u8; NUM_LANES]; 4]; 2];

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 3 << 8);
    for (channel, slotrank) in ranks.iter().copied() {
        find_read_mpr_margin(
            mchbar_base,
            state,
            channel,
            slotrank,
            &mut falling_edges[channel][slotrank],
        )?;
    }

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 2 << 8);
    for (channel, slotrank) in ranks.iter().copied() {
        find_read_mpr_margin(
            mchbar_base,
            state,
            channel,
            slotrank,
            &mut rising_edges[channel][slotrank],
        )?;
    }

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 0);
    for (channel, slotrank) in ranks.iter().copied() {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].rx_dqs_n =
                falling_edges[channel][slotrank][lane];
            state.training.ranks[channel][slotrank].lanes[lane].rx_dqs_p =
                rising_edges[channel][slotrank][lane];
        }
    }

    mchbar::program_rank_timings(mchbar_base, &state.training);
    for channel in channels.iter().copied() {
        for lane in 0..state.training.active_lanes {
            mchbar::write32(
                mchbar_base + cxly(IOSAV_BY_BW_MASK_CH_BASE, channel, lane),
                0,
            );
        }
    }

    Ok(())
}

/// Port of coreboot `aggressive_read_training()` for Sandy Bridge.
pub fn aggressive_train(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    let mut falling_edges = [[[0u8; NUM_LANES]; 4]; 2];
    let mut rising_edges = [[[0u8; NUM_LANES]; 4]; 2];

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 3 << 8);
    for (channel, slotrank) in ranks.iter().copied() {
        find_aggressive_read_margin(
            mchbar_base,
            state,
            channel,
            slotrank,
            &mut falling_edges[channel][slotrank],
        )?;
    }

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 2 << 8);
    for (channel, slotrank) in ranks.iter().copied() {
        find_aggressive_read_margin(
            mchbar_base,
            state,
            channel,
            slotrank,
            &mut rising_edges[channel][slotrank],
        )?;
    }

    mchbar::write32(mchbar_base + IOSAV_DC_MASK, 0);
    for (channel, slotrank) in ranks.iter().copied() {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].rx_dqs_n =
                falling_edges[channel][slotrank][lane];
            state.training.ranks[channel][slotrank].lanes[lane].rx_dqs_p =
                rising_edges[channel][slotrank][lane];
        }
    }
    mchbar::program_rank_timings(mchbar_base, &state.training);
    Ok(())
}

fn find_read_mpr_margin(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    edges: &mut [u8; NUM_LANES],
) -> Result<(), ServiceError> {
    let mut stats = [[0u32; (MAX_EDGE_TIMING as usize) + 1]; NUM_LANES];
    for dqs_pi in 0..=MAX_EDGE_TIMING {
        for lane in 0..state.training.active_lanes {
            let lane_timing = &mut state.training.ranks[channel][slotrank].lanes[lane];
            lane_timing.rx_dqs_p = dqs_pi;
            lane_timing.rx_dqs_n = dqs_pi;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        for lane in 0..state.training.active_lanes {
            mchbar::write32(
                mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane),
                0,
            );
            let _ = mchbar::read32(mchbar_base + cxly(IOSAV_BY_BW_SERROR_C_CH_BASE, channel, lane));
        }

        iosav::wait(mchbar_base, channel)?;
        iosav::write_read_mpr_sequence(
            mchbar_base,
            channel,
            slotrank as u8,
            state.timing.tmod as u16,
            500,
            4,
            1,
            u16::from(state.timing.cas) + 8,
        );
        iosav::run_once_and_wait(mchbar_base, channel, 4)?;

        for (lane, lane_stats) in stats
            .iter_mut()
            .enumerate()
            .take(state.training.active_lanes)
        {
            lane_stats[dqs_pi as usize] =
                mchbar::read32(mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane));
        }
    }

    for lane in 0..state.training.active_lanes {
        let run = longest_zero_run(&stats[lane]);
        if run.all {
            return Err(ServiceError::HardwareError);
        }
        edges[lane] = run.middle as u8;
    }
    Ok(())
}

fn find_predefined_pattern(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
) -> Result<(), ServiceError> {
    patterns::fill_pattern0(mchbar_base, state, channel, 0, 0);
    for lane in 0..state.training.active_lanes {
        mchbar::write32(
            mchbar_base + cxly(IOSAV_BY_BW_MASK_CH_BASE, channel, lane),
            0,
        );
        let _ = mchbar::read32(mchbar_base + cxly(IOSAV_BY_BW_SERROR_C_CH_BASE, channel, lane));
    }

    set_channel_read_dqs(state, channel, 16);
    mchbar::program_rank_timings(mchbar_base, &state.training);
    run_read_mpr_on_channel(mchbar_base, state, channel, 3)?;

    set_channel_read_dqs(state, channel, 48);
    mchbar::program_rank_timings(mchbar_base, &state.training);
    run_read_mpr_on_channel(mchbar_base, state, channel, 3)?;

    for lane in 0..state.training.active_lanes {
        let sticky = mchbar::read32(mchbar_base + cxly(IOSAV_BY_BW_SERROR_CH_BASE, channel, lane));
        mchbar::write32(
            mchbar_base + cxly(IOSAV_BY_BW_MASK_CH_BASE, channel, lane),
            (!sticky) & 0xff,
        );
    }
    Ok(())
}

fn set_channel_read_dqs(state: &mut RaminitState, channel: usize, value: u8) {
    for slotrank in 0..4 {
        if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
            continue;
        }
        for lane in 0..state.training.active_lanes {
            let lane_timing = &mut state.training.ranks[channel][slotrank].lanes[lane];
            lane_timing.rx_dqs_n = value;
            lane_timing.rx_dqs_p = value;
        }
    }
}

fn run_read_mpr_on_channel(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    loops: u16,
) -> Result<(), ServiceError> {
    for slotrank in 0..4 {
        if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
            continue;
        }
        iosav::wait(mchbar_base, channel)?;
        iosav::write_read_mpr_sequence(
            mchbar_base,
            channel,
            slotrank as u8,
            state.timing.tmod as u16,
            loops,
            4,
            1,
            u16::from(state.timing.cas) + 8,
        );
        iosav::run_once_and_wait(mchbar_base, channel, 4)?;
    }
    Ok(())
}

fn find_aggressive_read_margin(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
    edges: &mut [u8; NUM_LANES],
) -> Result<(), ServiceError> {
    let rd_vref_offsets = [0u32, 0x0c, 0x2c];
    let mut lower = [0usize; NUM_LANES];
    let mut upper = [MAX_EDGE_TIMING as usize; NUM_LANES];

    for (i, vref) in rd_vref_offsets.iter().copied().enumerate() {
        mchbar::write32(
            mchbar_base + gz(GDCRTRAININGMOD_CH_BASE, channel),
            vref << 24,
        );
        for pat in 0..patterns::NUM_PATTERNS {
            patterns::fill_pattern5(mchbar_base, state, channel, pat);
            let mut raw_stats = [0u32; (MAX_EDGE_TIMING as usize) + 1];
            for read_pi in 0..=MAX_EDGE_TIMING {
                for lane in 0..state.training.active_lanes {
                    let lane_timing = &mut state.training.ranks[channel][slotrank].lanes[lane];
                    lane_timing.rx_dqs_p = read_pi;
                    lane_timing.rx_dqs_n = read_pi;
                }
                mchbar::program_rank_timings(mchbar_base, &state.training);
                for lane in 0..state.training.active_lanes {
                    mchbar::write32(
                        mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane),
                        0,
                    );
                    let _ = mchbar::read32(
                        mchbar_base + cxly(IOSAV_BY_BW_SERROR_C_CH_BASE, channel, lane),
                    );
                }
                iosav::wait(mchbar_base, channel)?;
                iosav::write_data_write_sequence(
                    mchbar_base,
                    channel,
                    slotrank as u8,
                    iosav::CommandTrainingTiming {
                        trcd: state.timing.trcd as u16,
                        trrd: state.timing.trrd as u8,
                        tfaw: state.timing.tfaw as u8,
                        cwl: u16::from(state.timing.cwl),
                        twtr: state.timing.twtr as u16,
                        trtp: state.timing.trtp as u16,
                        trp: state.timing.trp as u16,
                    },
                );
                iosav::run_once_and_wait(mchbar_base, channel, 4)?;
                for lane in 0..state.training.active_lanes {
                    let _ = mchbar::read32(
                        mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane),
                    );
                }
                raw_stats[read_pi as usize] =
                    mchbar::read32(mchbar_base + cx(IOSAV_BYTE_SERROR_C_CH_BASE, channel));
            }

            for lane in 0..state.training.active_lanes {
                let mut stats = [0u32; (MAX_EDGE_TIMING as usize) + 1];
                for read_pi in 0..=MAX_EDGE_TIMING {
                    stats[read_pi as usize] =
                        u32::from((raw_stats[read_pi as usize] & (1 << lane)) != 0);
                }
                let run = longest_zero_run(&stats);
                if run.all {
                    mchbar::write32(mchbar_base + gz(GDCRTRAININGMOD_CH_BASE, channel), 0);
                    return Err(ServiceError::HardwareError);
                }
                lower[lane] =
                    lower[lane].max(run.start + usize::from(state.training.edge_offset[i]));
                upper[lane] = upper[lane].min(
                    run.end
                        .saturating_sub(usize::from(state.training.edge_offset[i])),
                );
                if lower[lane] > upper[lane] {
                    mchbar::write32(mchbar_base + gz(GDCRTRAININGMOD_CH_BASE, channel), 0);
                    return Err(ServiceError::HardwareError);
                }
                edges[lane] = ((lower[lane] + upper[lane]) / 2) as u8;
            }
        }
    }

    mchbar::write32(mchbar_base + gz(GDCRTRAININGMOD_CH_BASE, channel), 0);
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Run {
    start: usize,
    middle: usize,
    end: usize,
    all: bool,
}

fn longest_zero_run(seq: &[u32]) -> Run {
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
            end: sz.saturating_sub(1),
            all: true,
        };
    }
    Run {
        start: best_start % sz,
        middle: (best_start + (best_len - 1) / 2) % sz,
        end: (best_start + best_len - 1) % sz,
        all: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_zero_run_finds_center_of_error_free_window() {
        let seq = [1, 0, 0, 0, 1, 1];
        let run = longest_zero_run(&seq);
        assert_eq!(run.middle, 2);
        assert!(!run.all);
    }
}
