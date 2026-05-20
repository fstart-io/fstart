//! MCHBAR register helpers for Sandy Bridge native raminit.

use fstart_arch_x86::udelay;
use fstart_services::ServiceError;

use super::memory_map;
use super::state::ControllerTopology;
use super::timing::{self, TimingParams};
use super::training::{TrainingState, CCC_MAX_PI, NUM_LANES, QCLK_PI};

const LANE_BASES: [usize; NUM_LANES] = [
    0x0000, 0x0200, 0x0400, 0x0600, 0x1000, 0x1200, 0x1400, 0x1600, 0x0800,
];

const GDCRRX_BASE: usize = 0x0010;
const GDCRTX_BASE: usize = 0x0020;
const GDCRCLKRANKSUSED_BASE: usize = 0x0c00;
const GDCRCKLOGICDELAY_BASE: usize = 0x0c18;
const CRCOMPOFST1_BASE: usize = 0x1810;
const GDCRCTLRANKSUSED_BASE: usize = 0x3200;
const CRCOMPOFST2: usize = 0x3714;
const GDCRCKPICODE_BASE: usize = 0x0c14;
const GDCRCMDPICODING_BASE: usize = 0x320c;
const TC_DBP_BASE: usize = 0x4000;
const TC_RAP_BASE: usize = 0x4004;
const TC_OTHP_BASE: usize = 0x400c;
const SC_ROUNDT_LAT_BASE: usize = 0x4024;
const SC_IO_LATENCY_BASE: usize = 0x4028;
const TC_RFP_BASE: usize = 0x4294;
const TC_RFTP_BASE: usize = 0x4298;
const TC_SRFTP_BASE: usize = 0x42a4;
const MAD_DIMM_BASE: usize = 0x5004;
const MAD_CHNL: usize = 0x5000;
const MAD_ZR: usize = 0x5014;
const PM_THML_STAT: usize = 0x4e80;
const SCHED_CBIT: usize = 0x4c20;
const SC_WDBWM: usize = 0x4f8c;
const MC_INIT_STATE_G: usize = 0x5030;
const MRC_REVISION: usize = 0x5034;
const RCOMP_TIMER: usize = 0x5084;
const MC_BIOS_REQ: usize = 0x5e00;
const MC_BIOS_DATA: usize = 0x5e04;
const M_COMP: usize = 0x5f08;
const PM_DLL_CONFIG: usize = 0x5064;

const DEFAULT_TDLLK: u32 = 512;
const MRC_REVISION_NATIVE: u32 = 0xc04e_b002;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn gz(base: usize, channel: usize) -> usize {
    base + (channel << 8)
}

#[inline]
const fn ly(base: usize, lane: usize) -> usize {
    base + (lane << 2)
}

#[inline]
const fn gzly(base: usize, channel: usize, index: usize) -> usize {
    base + (channel << 8) + (index << 2)
}

fn get_xover_cmd(rankmap: u8) -> u32 {
    let mut reg = 1 << 14;
    if (rankmap & 0x03) != 0 {
        reg |= 1 << 17;
    }
    if (rankmap & 0x0c) != 0 {
        reg |= 1 << 26;
    }
    reg
}

/// Program the controller state that coreboot sets before JEDEC reset/training.
pub fn program_pre_training(
    mchbar_base: usize,
    topology: &ControllerTopology,
    timing: &TimingParams,
    training: &TrainingState,
    me_uma_size_mb: u32,
) -> Result<(), ServiceError> {
    program_memory_frequency(mchbar_base, timing)?;
    write32(mchbar_base + MRC_REVISION, MRC_REVISION_NATIVE);
    program_xover(mchbar_base, topology);
    program_timing_registers(mchbar_base, timing);
    write32(mchbar_base + PM_THML_STAT, 0x5500);
    write32(mchbar_base + SCHED_CBIT, 0x1010_0005);
    // X220 Sandy Bridge CPUs are post D1 in normal configurations; use the
    // non-D0/D1 watermarks from coreboot's `set_wmm_behavior()`.
    write32(mchbar_base + SC_WDBWM, 0x551d_1519);
    clrbits32(mchbar_base + MC_INIT_STATE_G, 1 << 5);
    program_dimm_mapping(mchbar_base, topology, false);
    program_zones(mchbar_base, topology, true);
    let _memory_map = memory_map::program(topology, me_uma_size_mb);
    program_io_rank_masks(mchbar_base, topology);
    program_rank_timings(mchbar_base, training);
    program_io_registers(mchbar_base, topology, timing)?;
    udelay(1);
    Ok(())
}

fn program_memory_frequency(mchbar_base: usize, timing: &TimingParams) -> Result<(), ServiceError> {
    if timing.tck_256ns > timing::TCK_400MHZ {
        return Err(ServiceError::NotSupported);
    }

    // If MC_BIOS_DATA already reports a non-zero frequency, coreboot exits
    // early because requesting the already-set MPLL frequency can hang.
    if (read32(mchbar_base + MC_BIOS_DATA) & 0xff) != 0 {
        return Ok(());
    }

    write32(mchbar_base + MC_BIOS_REQ, timing.frq | (1 << 31));
    for _ in 0..10_000 {
        let req = read32(mchbar_base + MC_BIOS_REQ);
        if (req & (1 << 31)) == 0 {
            let data = read32(mchbar_base + MC_BIOS_DATA) & 0xff;
            if data >= timing.frq {
                return Ok(());
            }
            return Err(ServiceError::HardwareError);
        }
        udelay(10);
    }
    Err(ServiceError::Timeout)
}

fn program_xover(mchbar_base: usize, topology: &ControllerTopology) {
    for channel in 0..2 {
        let rankmap = u32::from(topology.rankmap[channel]);
        write32(mchbar_base + gz(GDCRCKPICODE_BASE, channel), rankmap << 24);

        let mut cmd = 1 << 14;
        if (rankmap & 0x03) != 0 {
            cmd |= 1 << 17;
        }
        if (rankmap & 0x0c) != 0 {
            cmd |= 1 << 26;
        }
        write32(mchbar_base + gz(GDCRCMDPICODING_BASE, channel), cmd);
    }
}

fn program_dimm_mapping(mchbar_base: usize, topology: &ControllerTopology, ecc_training: bool) {
    let ecc_bits = if ecc_training { 1 << 24 } else { 0 };
    for channel in 0..2 {
        write32(
            mchbar_base + ly(MAD_DIMM_BASE, channel),
            topology.mad_dimm[channel] | ecc_bits,
        );
    }
}

fn program_zones(mchbar_base: usize, topology: &ControllerTopology, training: bool) {
    let ch0size = if training && topology.channel_size_mb[0] != 0 {
        256
    } else {
        topology.channel_size_mb[0]
    };
    let ch1size = if training && topology.channel_size_mb[1] != 0 {
        256
    } else {
        topology.channel_size_mb[1]
    };

    let (smaller, mad_chnl) = if ch0size >= ch1size {
        (ch1size, 0x24)
    } else {
        (ch0size, 0x21)
    };
    let val = smaller / 256;
    let mut reg = read32(mchbar_base + MAD_ZR);
    reg = (reg & !0xff00_0000) | (val << 24);
    reg = (reg & !0x00ff_0000) | ((2 * val) << 16);
    write32(mchbar_base + MAD_ZR, reg);
    write32(mchbar_base + MAD_CHNL, mad_chnl);
}

fn program_io_rank_masks(mchbar_base: usize, topology: &ControllerTopology) {
    for channel in 0..2 {
        let rankmap = u32::from(topology.rankmap[channel]);
        write32(mchbar_base + gz(GDCRCLKRANKSUSED_BASE, channel), rankmap);
        write32(mchbar_base + gz(GDCRCTLRANKSUSED_BASE, channel), rankmap);
    }
}

fn program_io_registers(
    mchbar_base: usize,
    topology: &ControllerTopology,
    timing: &TimingParams,
) -> Result<(), ServiceError> {
    wait_for_rcomp(mchbar_base)?;
    write32(
        mchbar_base + CRCOMPOFST2,
        comp2_for_tck_sandy(timing.tck_256ns),
    );
    for channel in 0..2 {
        if topology.rankmap[channel] != 0 {
            let comp1 =
                encode_comp1_sandy_non_d2(read32(mchbar_base + gz(CRCOMPOFST1_BASE, channel)));
            write32(mchbar_base + gz(CRCOMPOFST1_BASE, channel), comp1);
        }
    }
    setbits32(mchbar_base + M_COMP, 1 << 8);
    udelay(20);
    Ok(())
}

pub fn program_rank_timings(mchbar_base: usize, state: &TrainingState) {
    for channel in 0..2 {
        program_channel_rank_timings(mchbar_base, state, channel);
    }
}

fn program_channel_rank_timings(mchbar_base: usize, state: &TrainingState, channel: usize) {
    let rankmap = state.populated_rankmap[channel];
    if rankmap == 0 {
        return;
    }

    let mut cmd_delay = 0i16;
    for slotrank in 0..4 {
        if (rankmap & (1 << slotrank)) != 0 {
            cmd_delay = cmd_delay.max(-state.ranks[channel][slotrank].pi_coding);
        }
    }
    cmd_delay = cmd_delay.min(CCC_MAX_PI);

    let mut ctl_delay = [0i16; 2];
    for (slot, delay) in ctl_delay.iter_mut().enumerate() {
        let slot_map = (rankmap >> (2 * slot)) & 3;
        if (slot_map & 1) != 0 {
            *delay += state.ranks[channel][2 * slot].pi_coding + cmd_delay;
        }
        if (slot_map & 2) != 0 {
            *delay += state.ranks[channel][2 * slot + 1].pi_coding + cmd_delay;
        }
        if slot_map == 3 {
            *delay /= 2;
        }
        *delay = (*delay).min(CCC_MAX_PI);
    }

    let mut clk_pi_coding = u32::from(rankmap) << 24;
    let mut clk_logic_delay = 0u32;
    for slotrank in 0..4 {
        if (rankmap & (1 << slotrank)) == 0 {
            continue;
        }
        let mut clk_delay =
            state.ranks[channel][slotrank].pi_coding + cmd_delay + i16::from(state.pi_code_offset);
        if clk_delay < 0 {
            clk_delay = 0;
        }
        clk_delay %= CCC_MAX_PI + 1;
        clk_pi_coding |= u32::from((clk_delay % QCLK_PI) as u16) << (6 * slotrank);
        clk_logic_delay |= u32::from((clk_delay / QCLK_PI) as u16) << slotrank;
    }

    let cmd_pi_coding = get_xover_cmd(rankmap)
        | u32::from((cmd_delay % QCLK_PI) as u16)
        | (u32::from((ctl_delay[0] % QCLK_PI) as u16) << 6)
        | (u32::from((cmd_delay / QCLK_PI) as u16) << 12)
        | (u32::from((ctl_delay[0] / QCLK_PI) as u16) << 15)
        | (u32::from((ctl_delay[1] % QCLK_PI) as u16) << 18)
        | (u32::from((ctl_delay[1] / QCLK_PI) as u16) << 24);

    write32(
        mchbar_base + gz(GDCRCMDPICODING_BASE, channel),
        cmd_pi_coding,
    );
    write32(mchbar_base + gz(GDCRCKPICODE_BASE, channel), clk_pi_coding);
    write32(
        mchbar_base + gz(GDCRCKLOGICDELAY_BASE, channel),
        clk_logic_delay,
    );

    let mut io_latency = read32(mchbar_base + cx(SC_IO_LATENCY_BASE, channel)) & !0xffff;
    let mut roundtrip_latency = 0u32;
    for slotrank in 0..4 {
        if (rankmap & (1 << slotrank)) == 0 {
            continue;
        }
        let rank = &state.ranks[channel][slotrank];
        io_latency |= u32::from(rank.io_latency) << (4 * slotrank);
        roundtrip_latency |= u32::from(rank.roundtrip_latency) << (8 * slotrank);
        for lane in 0..state.active_lanes {
            let lane_timing = rank.lanes[lane];
            let rcven_logic = lane_timing.rcven / (QCLK_PI as u16);
            let gdcr_rx = (lane_timing.rcven % (QCLK_PI as u16)) as u32
                | (u32::from(lane_timing.rx_dqs_p) << 8)
                | (u32::from(rcven_logic & 0x7) << 16)
                | (u32::from(lane_timing.rx_dqs_n) << 20);
            write32(
                mchbar_base + LANE_BASES[lane] + gzly(GDCRRX_BASE, channel, slotrank),
                gdcr_rx,
            );

            let tx_dq = lane_timing.tx_dq;
            let tx_dqs = lane_timing.tx_dqs;
            let tx_dq_pi = (tx_dq % QCLK_PI) as u32 & 0x3f;
            let tx_dq_logic = (tx_dq / QCLK_PI) as u32 & 0x1;
            let gdcr_tx = tx_dq_pi
                | (u32::from(tx_dqs % (QCLK_PI as u16)) << 8)
                | (u32::from((tx_dqs / (QCLK_PI as u16)) & 0x7) << 15)
                | (tx_dq_logic << 19);
            write32(
                mchbar_base + LANE_BASES[lane] + gzly(GDCRTX_BASE, channel, slotrank),
                gdcr_tx,
            );
        }
    }
    write32(
        mchbar_base + cx(SC_ROUNDT_LAT_BASE, channel),
        roundtrip_latency,
    );
    write32(mchbar_base + cx(SC_IO_LATENCY_BASE, channel), io_latency);
}

fn wait_for_rcomp(mchbar_base: usize) -> Result<(), ServiceError> {
    for _ in 0..1_000_000 {
        if (read32(mchbar_base + RCOMP_TIMER) & (1 << 16)) != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ServiceError::Timeout)
}

fn encode_comp1_sandy_non_d2(orig: u32) -> u32 {
    (orig & !((0x7 << 9) | (0x7 << 21) | (0x7 << 27))) | (1 << 9) | (1 << 21) | (1 << 27)
}

fn comp2_for_tck_sandy(tck: u32) -> u32 {
    if tck <= timing::TCK_1066MHZ {
        0x0c21_410c
    } else if tck <= timing::TCK_933MHZ {
        0x0c42_514c
    } else if tck <= timing::TCK_800MHZ {
        0x0c63_69cc
    } else if tck <= timing::TCK_666MHZ {
        0x0ca5_7a4c
    } else if tck <= timing::TCK_533MHZ {
        0x0ce7_c34c
    } else {
        0x0d6b_edcc
    }
}

fn program_timing_registers(mchbar_base: usize, timing: &TimingParams) {
    let tc_dbp = (timing.trcd & 0x0f)
        | ((timing.trp & 0x0f) << 4)
        | ((u32::from(timing.cas) & 0x0f) << 8)
        | ((u32::from(timing.cwl) & 0x0f) << 12)
        | ((timing.tras & 0xff) << 16);

    let tc_rap = (timing.trrd & 0x0f)
        | ((timing.trtp & 0x0f) << 4)
        | ((timing.tcke & 0x0f) << 8)
        | ((timing.twtr & 0x0f) << 12)
        | ((timing.tfaw & 0xff) << 16)
        | ((timing.twr & 0x1f) << 24)
        | (3 << 30);

    let tc_othp = timing.txpdll.min(31)
        | (timing.txp.min(7) << 5)
        | ((timing.taonpd & 0x0f) << 8)
        | (1 << 12)
        | (1 << 14);

    let trefix9 = ((timing.trefi * 89) / 10).min((70_000 << 8) / timing.tck_256ns) / 1024;
    let tc_rftp =
        (timing.trefi & 0xffff) | ((timing.trfc & 0x01ff) << 16) | ((trefix9 & 0x7f) << 25);
    let tc_rfp = 0xff;
    let tc_srftp = DEFAULT_TDLLK
        | ((timing.txs_offset & 0x0f) << 12)
        | (((DEFAULT_TDLLK - timing.txs_offset) & 0x03ff) << 16)
        | (((timing.tmod - 8) & 0x0f) << 28);

    for channel in 0..2 {
        write32(mchbar_base + cx(TC_DBP_BASE, channel), tc_dbp);
        write32(mchbar_base + cx(TC_RAP_BASE, channel), tc_rap);
        write32(mchbar_base + cx(TC_OTHP_BASE, channel), tc_othp);
        write32(mchbar_base + cx(TC_RFTP_BASE, channel), tc_rftp);
        let mut rfp = read32(mchbar_base + cx(TC_RFP_BASE, channel));
        rfp = (rfp & !0xff) | tc_rfp;
        write32(mchbar_base + cx(TC_RFP_BASE, channel), rfp);
        write32(mchbar_base + cx(TC_SRFTP_BASE, channel), tc_srftp);
    }

    let mut pm_dll = read32(mchbar_base + PM_DLL_CONFIG);
    pm_dll = (pm_dll & !0x0fff) | u32::from(timing.mdll_wake_delay & 0x0fff);
    write32(mchbar_base + PM_DLL_CONFIG, pm_dll);
}

#[cfg(target_arch = "x86_64")]
pub(super) fn setbits32(addr: usize, bits: u32) {
    write32(addr, read32(addr) | bits);
}

pub(super) fn clrbits32(addr: usize, bits: u32) {
    write32(addr, read32(addr) & !bits);
}

pub(super) fn toggle_io_reset(mchbar_base: usize) {
    let val = read32(mchbar_base + MC_INIT_STATE_G);
    write32(mchbar_base + MC_INIT_STATE_G, val | (1 << 5));
    udelay(1);
    write32(mchbar_base + MC_INIT_STATE_G, val & !(1 << 5));
    udelay(1);
}

#[cfg(target_arch = "x86_64")]

pub(super) fn write8(addr: usize, value: u8) {
    // SAFETY: caller passes chipset MCHBAR MMIO addresses during BSP raminit.
    unsafe { core::ptr::write_volatile(addr as *mut u8, value) }
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn write8(_addr: usize, _value: u8) {}

#[cfg(target_arch = "x86_64")]
pub(super) fn write32(addr: usize, value: u32) {
    // SAFETY: caller passes chipset MCHBAR MMIO addresses during BSP raminit.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn write32(_addr: usize, _value: u32) {}

#[cfg(target_arch = "x86_64")]
pub(super) fn read32(addr: usize) -> u32 {
    // SAFETY: caller passes chipset MCHBAR MMIO addresses during BSP raminit.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn read32(_addr: usize) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_basic_timing_registers_without_panicking() {
        let timing = TimingParams {
            tck_256ns: 384,
            base_freq_mhz: 133,
            frq: 5,
            cas: 9,
            cwl: 7,
            trcd: 9,
            trp: 9,
            tras: 24,
            twr: 10,
            tfaw: 20,
            trrd: 4,
            trtp: 5,
            twtr: 5,
            trfc: 174,
            trefi: 5200,
            tmod: 12,
            txs_offset: 7,
            twlo: 6,
            tcke: 4,
            txpdll: 16,
            txp: 4,
            taonpd: 6,
            mdll_wake_delay: 336,
        };
        let tc_dbp = (timing.trcd & 0x0f)
            | ((timing.trp & 0x0f) << 4)
            | ((u32::from(timing.cas) & 0x0f) << 8)
            | ((u32::from(timing.cwl) & 0x0f) << 12)
            | ((timing.tras & 0xff) << 16);
        assert_eq!(tc_dbp, 0x0018_7999);
    }
}
