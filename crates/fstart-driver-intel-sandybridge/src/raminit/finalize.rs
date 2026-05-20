//! Final Sandy Bridge native raminit programming after training.

use fstart_services::ServiceError;

use super::iosav;
use super::mchbar;
use super::state::ControllerTopology;
use super::RaminitState;

const TC_RWP_BASE: usize = 0x4008;
const TC_RAP_BASE: usize = 0x4004;
const MC_INIT_STATE_CH_BASE: usize = 0x42a0;
const IOSAV_DATA_CTL_CH_BASE: usize = 0x4288;
const IOSAV_BY_BW_SERROR_C: usize = 0x4d40;
const IOSAV_BY_ERROR_COUNT: usize = 0x4f40;
const IOSAV_BY_ERROR_COUNT_CH_BASE: usize = 0x4340;
const PM_PDWN_CONFIG_CH_BASE: usize = 0x40b0;
const PM_TRML_M_CONFIG_CH_BASE: usize = 0x4380;
const TC_RFP_BASE: usize = 0x4294;
const MAD_DIMM_BASE: usize = 0x5004;
const MAD_CHNL: usize = 0x5000;
const MAD_ZR: usize = 0x5014;
const SCRAMBLING_SEED_1_CH_BASE: usize = 0x4034;
const SCRAMBLING_SEED_2_LO_CH_BASE: usize = 0x4038;
const SCRAMBLING_SEED_2_HI_CH_BASE: usize = 0x403c;
const SCHED_CBIT_CH_BASE: usize = 0x4020;
const CHANNEL_HASH: usize = 0x5024;
const MC_INIT_STATE_G: usize = 0x5030;
const PM_BW_LIMIT_CONFIG: usize = 0x4f88;
const PM_DLL_CONFIG: usize = 0x5064;
const PM_CMD_PWR_CH_BASE: usize = 0x4384;
const MEM_TRML_ESTIMATION_CONFIG: usize = 0x5880;
const MEM_TRML_THRESHOLDS_CONFIG: usize = 0x5888;
const MEM_TRML_INTERRUPT: usize = 0x58a8;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn ly(base: usize, lane: usize) -> usize {
    base + (lane << 2)
}

#[inline]
const fn cxly(base: usize, channel: usize, lane: usize) -> usize {
    base + (channel << 10) + (lane << 2)
}

pub fn normalize_training(mchbar_base: usize, state: &mut RaminitState) {
    let ranks: heapless::Vec<_, 8> = state.training.populated_ranks().collect();
    for (channel, slotrank) in ranks {
        let mut max_rcven = 0u16;
        for lane in 0..state.training.active_lanes {
            max_rcven = max_rcven.max(state.training.ranks[channel][slotrank].lanes[lane].rcven);
        }
        let delta =
            (max_rcven >> 6) as i16 - i16::from(state.training.ranks[channel][slotrank].io_latency);
        let rank = &mut state.training.ranks[channel][slotrank];
        rank.roundtrip_latency = (i16::from(rank.roundtrip_latency) + delta).max(0) as u8;
        rank.io_latency = (i16::from(rank.io_latency) + delta).max(0) as u8;
    }
    mchbar::program_rank_timings(mchbar_base, &state.training);
}

pub fn channel_test(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0
            && (mchbar::read32(mchbar_base + cx(MC_INIT_STATE_CH_BASE, channel)) & 0xa000) != 0
        {
            return Err(ServiceError::HardwareError);
        }
    }
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            fill_pattern0(mchbar_base, state, channel, 0x1234_5678, 0x9876_5432);
        }
    }
    for slotrank in 0..4 {
        for channel in 0..2 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
                continue;
            }
            for lane in 0..state.training.active_lanes {
                mchbar::write32(mchbar_base + ly(IOSAV_BY_ERROR_COUNT, lane), 0);
                mchbar::write32(mchbar_base + ly(IOSAV_BY_BW_SERROR_C, lane), 0);
            }
            iosav::wait(mchbar_base, channel)?;
            iosav::write_memory_test_sequence(mchbar_base, channel, slotrank as u8);
            iosav::run_once_and_wait(mchbar_base, channel, 4)?;
            for lane in 0..state.training.active_lanes {
                if mchbar::read32(mchbar_base + cxly(IOSAV_BY_ERROR_COUNT_CH_BASE, channel, lane))
                    != 0
                {
                    return Err(ServiceError::HardwareError);
                }
            }
        }
    }
    Ok(())
}

pub fn set_read_write(mchbar_base: usize, state: &RaminitState) {
    set_read_write_timings(mchbar_base, state);
}

pub fn final_programming(mchbar_base: usize, state: &RaminitState) {
    set_final_dimm_mapping(mchbar_base, &state.topology);
    set_channel_hash_and_scrambling(mchbar_base, state);
    set_normal_operation(mchbar_base, state);
    final_registers(mchbar_base, state);
    program_zones(mchbar_base, &state.topology, false);
}

fn fill_pattern0(mchbar_base: usize, state: &RaminitState, channel: usize, a: u32, b: u32) {
    let base = 0x0400_0000 + preceding_channels(state, channel) * 64;
    for j in 0..16usize {
        let val = if (j & 2) != 0 { b } else { a };
        // SAFETY: coreboot uses this WDB staging aperture during IOSAV tests.
        unsafe { core::ptr::write_volatile((base + j * 4) as *mut u32, val) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    mchbar::write8(mchbar_base + cx(IOSAV_DATA_CTL_CH_BASE, channel), 0);
}

fn preceding_channels(state: &RaminitState, target_channel: usize) -> usize {
    (0..target_channel)
        .filter(|channel| state.topology.rankmap[*channel] != 0)
        .count()
}

fn set_read_write_timings(mchbar_base: usize, state: &RaminitState) {
    let trwdrdd_inc = if state.timing.tck_256ns <= super::timing::TCK_1066MHZ {
        4
    } else {
        2
    };
    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        let mut min_pi = i16::MAX;
        let mut max_pi = i16::MIN;
        for slotrank in 0..4 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) != 0 {
                let pi = state.training.ranks[channel][slotrank].pi_coding;
                min_pi = min_pi.min(pi);
                max_pi = max_pi.max(pi);
            }
        }
        let twrdrdd = if max_pi - min_pi > 51 {
            0
        } else {
            state.training.ref_card_offset[channel]
        };
        let val = if i16::from(state.training.pi_coding_threshold) < max_pi - min_pi {
            3
        } else {
            2
        };
        let tc_rwp = (val << 4)
            | (val << 8)
            | (val << 12)
            | ((u32::from(state.training.ref_card_offset[channel]) + trwdrdd_inc) << 16)
            | (u32::from(twrdrdd) << 20)
            | (2 << 24)
            | (1 << 27);
        mchbar::write32(mchbar_base + cx(TC_RWP_BASE, channel), tc_rwp);
    }
}

fn set_channel_hash_and_scrambling(mchbar_base: usize, state: &RaminitState) {
    // Coreboot programs CHANNEL_HASH unconditionally even though the register is
    // documented as IVB-only; keep the same value for parity with native raminit.
    mchbar::write32(mchbar_base + CHANNEL_HASH, 0x00a0_30ce);
    let seeds = [
        [0x0000_9a36, 0xbafc_fdcf, 0x46d1_ab68],
        [0x0002_8bfa, 0x53fe_4b49, 0x19ed_5483],
    ];
    for (channel, seed) in seeds.iter().enumerate() {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        mchbar::clrbits32(mchbar_base + cx(SCHED_CBIT_CH_BASE, channel), 1 << 28);
        mchbar::write32(
            mchbar_base + cx(SCRAMBLING_SEED_1_CH_BASE, channel),
            seed[0],
        );
        mchbar::write32(
            mchbar_base + cx(SCRAMBLING_SEED_2_HI_CH_BASE, channel),
            seed[1],
        );
        mchbar::write32(
            mchbar_base + cx(SCRAMBLING_SEED_2_LO_CH_BASE, channel),
            seed[2],
        );
    }
}

fn set_normal_operation(mchbar_base: usize, state: &RaminitState) {
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            mchbar::write32(
                mchbar_base + cx(MC_INIT_STATE_CH_BASE, channel),
                (1 << 12) | u32::from(state.topology.rankmap[channel]),
            );
            mchbar::clrbits32(mchbar_base + cx(TC_RAP_BASE, channel), 1 << 29);
        }
    }
}

fn set_final_dimm_mapping(mchbar_base: usize, topology: &ControllerTopology) {
    for channel in 0..2 {
        mchbar::write32(
            mchbar_base + ly(MAD_DIMM_BASE, channel),
            topology.mad_dimm[channel],
        );
    }
}

fn program_zones(mchbar_base: usize, topology: &ControllerTopology, training: bool) {
    let ch0 = if training && topology.channel_size_mb[0] != 0 {
        256
    } else {
        topology.channel_size_mb[0]
    };
    let ch1 = if training && topology.channel_size_mb[1] != 0 {
        256
    } else {
        topology.channel_size_mb[1]
    };
    let (smaller, mad) = if ch0 >= ch1 { (ch1, 0x24) } else { (ch0, 0x21) };
    let val = smaller / 256;
    let mut reg = mchbar::read32(mchbar_base + MAD_ZR);
    reg = (reg & !0xff00_0000) | (val << 24);
    reg = (reg & !0x00ff_0000) | ((2 * val) << 16);
    mchbar::write32(mchbar_base + MAD_ZR, reg);
    mchbar::write32(mchbar_base + MAD_CHNL, mad);
}

fn final_registers(mchbar_base: usize, state: &RaminitState) {
    for channel in 0..2 {
        let rankmap = state.topology.rankmap[channel];
        mchbar::write32(
            mchbar_base + cx(PM_PDWN_CONFIG_CH_BASE, channel),
            (1 << 8) | 64,
        );
        mchbar::write32(
            mchbar_base + cx(PM_TRML_M_CONFIG_CH_BASE, channel),
            0x0000_0aaa,
        );
        let cmd_pwr = match rankmap {
            0 => 0,
            1 | 4 | 5 => 0x0037_3131,
            _ => 0x009b_6ea1,
        };
        mchbar::write32(mchbar_base + cx(PM_CMD_PWR_CH_BASE, channel), cmd_pwr);
        let mut rfp = mchbar::read32(mchbar_base + cx(TC_RFP_BASE, channel));
        rfp |= 1 << 8;
        mchbar::write32(mchbar_base + cx(TC_RFP_BASE, channel), rfp);
    }
    mchbar::write32(mchbar_base + PM_BW_LIMIT_CONFIG, 0x5f70_03ff);
    mchbar::write32(mchbar_base + PM_DLL_CONFIG, 0x0003_30f0);
    mchbar::write32(mchbar_base + MEM_TRML_ESTIMATION_CONFIG, 0xca91_71e5);
    let mut thresholds = mchbar::read32(mchbar_base + MEM_TRML_THRESHOLDS_CONFIG);
    thresholds = (thresholds & !0x00ff_ffff) | 0x00e4_d5d0;
    mchbar::write32(mchbar_base + MEM_TRML_THRESHOLDS_CONFIG, thresholds);
    mchbar::clrbits32(mchbar_base + MEM_TRML_INTERRUPT, 0x1f);
    mchbar::setbits32(mchbar_base + MC_INIT_STATE_G, (1 << 0) | (1 << 7));
}

#[cfg(test)]
mod tests {
    #[test]
    fn tc_rwp_basic_pack_shape() {
        let ref_card = 0u32;
        let val = 2u32;
        let packed =
            (val << 4) | (val << 8) | (val << 12) | ((ref_card + 2) << 16) | (2 << 24) | (1 << 27);
        assert_eq!(packed & (1 << 27), 1 << 27);
    }
}
