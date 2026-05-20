//! Command/address training for Sandy Bridge native raminit.

use fstart_services::ServiceError;

use super::iosav;
use super::jedec;
use super::mchbar;
use super::patterns;
use super::training::CCC_MAX_PI;
use super::RaminitState;

const IOSAV_BY_ERROR_COUNT_CH_BASE: usize = 0x4340;
const IOSAV_BY_ERROR_COUNT: usize = 0x4f40;
const IOSAV_DATA_CTL_CH_BASE: usize = 0x4288;
const IOSAV_N_ADDRESS_LFSR_CH_BASE: usize = 0x4240;
const MC_INIT_STATE_G: usize = 0x5030;
const SCHED_CBIT_CH_BASE: usize = 0x4020;
const TC_RAP_BASE: usize = 0x4004;
const CT_MIN_PI: i16 = -CCC_MAX_PI;
const CT_MAX_PI: i16 = CCC_MAX_PI + 1;
const CT_PI_LENGTH: usize = (CT_MAX_PI - CT_MIN_PI + 1) as usize;
const MIN_C320C_LEN: usize = 13;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn cxly(base: usize, channel: usize, index: usize) -> usize {
    base + (channel << 10) + (index << 2)
}

#[inline]
const fn ly(base: usize, index: usize) -> usize {
    base + (index << 2)
}

pub fn train(mchbar_base: usize, state: &mut RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            patterns::fill_pattern5(mchbar_base, state, channel, 0);
        }
    }

    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        let start_cmdrate = u8::from((state.topology.rankmap[channel] & 0x5) == 0x5);
        let mut ok = false;
        for cmdrate in start_cmdrate..2 {
            if try_cmd_stretch(mchbar_base, state, channel, cmdrate << 1).is_ok() {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(ServiceError::HardwareError);
        }
    }

    mchbar::program_rank_timings(mchbar_base, &state.training);
    reprogram_320c(mchbar_base, state)
}

fn try_cmd_stretch(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    cmd_stretch: u8,
) -> Result<(), ServiceError> {
    let saved = state.training.ranks[channel];
    state.training.cmd_stretch[channel] = cmd_stretch;
    write_tc_rap_cmd_stretch(mchbar_base, state, channel, cmd_stretch);

    let delta = match cmd_stretch {
        2 => 2,
        0 => 4,
        _ => 0,
    };
    for slotrank in 0..4 {
        if (state.topology.rankmap[channel] & (1 << slotrank)) != 0 {
            state.training.ranks[channel][slotrank].roundtrip_latency = state.training.ranks
                [channel][slotrank]
                .roundtrip_latency
                .saturating_sub(delta);
        }
    }

    let mut stat = [[0u8; CT_PI_LENGTH]; 4];
    for command_pi in (0..(CT_PI_LENGTH - 1)).map(|idx| CT_MIN_PI + idx as i16) {
        for slotrank in 0..4 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) != 0 {
                state.training.ranks[channel][slotrank].pi_coding = command_pi;
            }
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        reprogram_320c(mchbar_base, state)?;
        for slotrank in 0..4 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) != 0 {
                stat[slotrank][(command_pi - CT_MIN_PI) as usize] = u8::from(
                    test_command_training(mchbar_base, state, channel, slotrank)?,
                );
            }
        }
    }

    for (slotrank, slot_stat) in stat.iter().enumerate() {
        if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
            continue;
        }
        let run = longest_zero_run(&slot_stat[..CT_PI_LENGTH - 1]);
        state.training.ranks[channel][slotrank].pi_coding = run.middle as i16 + CT_MIN_PI;
        if run.all || run.length < MIN_C320C_LEN {
            state.training.ranks[channel] = saved;
            return Err(ServiceError::HardwareError);
        }
    }
    Ok(())
}

fn write_tc_rap_cmd_stretch(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    cmd_stretch: u8,
) {
    let timing = &state.timing;
    let tc_rap = (timing.trrd & 0x0f)
        | ((timing.trtp & 0x0f) << 4)
        | ((timing.tcke & 0x0f) << 8)
        | ((timing.twtr & 0x0f) << 12)
        | ((timing.tfaw & 0xff) << 16)
        | ((timing.twr & 0x1f) << 24)
        | ((u32::from(cmd_stretch) & 0x3) << 30);
    mchbar::write32(mchbar_base + cx(TC_RAP_BASE, channel), tc_rap);
}

fn test_command_training(
    mchbar_base: usize,
    state: &mut RaminitState,
    channel: usize,
    slotrank: usize,
) -> Result<bool, ServiceError> {
    let saved = state.training.ranks[channel][slotrank];
    let mut lanes_ok = 0u16;
    let target = (1u16 << state.training.active_lanes) - 1;
    let mut ctr = 0u16;

    for tx_dq_delta in -5..=5 {
        for lane in 0..state.training.active_lanes {
            state.training.ranks[channel][slotrank].lanes[lane].tx_dq =
                saved.lanes[lane].tx_dq + tx_dq_delta;
        }
        mchbar::program_rank_timings(mchbar_base, &state.training);
        for lane in 0..state.training.active_lanes {
            mchbar::write32(mchbar_base + ly(IOSAV_BY_ERROR_COUNT, lane), 0);
        }
        mchbar::write32(mchbar_base + cx(IOSAV_DATA_CTL_CH_BASE, channel), 0x1f);
        iosav::wait(mchbar_base, channel)?;
        iosav::write_command_training_sequence(
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
            ctr,
        );
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_ADDRESS_LFSR_CH_BASE, channel, 1),
            0x0389_abcd,
        );
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_ADDRESS_LFSR_CH_BASE, channel, 2),
            0x0389_abcd,
        );
        iosav::run_once_and_wait(mchbar_base, channel, 4)?;
        for lane in 0..state.training.active_lanes {
            if mchbar::read32(mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane)) == 0
            {
                lanes_ok |= 1 << lane;
            }
        }
        ctr += 1;
        if lanes_ok == target {
            break;
        }
    }

    state.training.ranks[channel][slotrank] = saved;
    Ok(lanes_ok != target)
}

fn reprogram_320c(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    disable_refresh_machine(mchbar_base, state)?;
    jedec::initialize_dram(mchbar_base, state)?;
    mchbar::toggle_io_reset(mchbar_base);
    Ok(())
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

#[derive(Clone, Copy, Default)]
struct Run {
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
            middle: sz / 2,
            length: sz,
            all: true,
        };
    }
    Run {
        middle: (best_start + (best_len - 1) / 2) % sz,
        length: best_len,
        all: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_zero_run_detects_middle_and_length() {
        let run = longest_zero_run(&[1, 0, 0, 0, 1]);
        assert_eq!(run.middle, 2);
        assert_eq!(run.length, 3);
        assert!(!run.all);
    }
}
