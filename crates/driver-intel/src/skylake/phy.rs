// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 PHY setup from native/mc_reg.c:calc_vt_slope_code,
//! program_fub_controls, program_ranks_used, program_data_train_defaults,
//! program_initial_slew_comp, program_initial_drive_comp, meminit_program_mc_regs.
//! RX Vref prerequisites are from sense_amp.c; forced RCOMP is from
//! si_optimization.c:meminit_force_rcomp. This does NOT train memory.

use super::{
    Error,
    capabilities::MemoryConfiguration,
    cpu::{Cpu, Family, Package},
    jedec::{BoardType, ModeRegisters, termination},
    phy_registers::*,
    registers::MchRegisters,
    reut::rank_mask,
    spd::Dimms,
    timings::Timings,
    training_state::TrainingOutput,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Board-supplied electrical/routing policy, not inferred from SPD. VCCIO
/// must already be resolved by platform code (power_model.c uses 950 mV or
/// 850 mV depending on ULX/ULT DMIBAR OPIO straps, unless overridden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhyPolicy {
    pub board: BoardType,
    pub dq_pins_interleaved: bool,
    pub ddp_shared_clock: bool,
    pub vdd_mv: u16,
    pub vccio_mv: u16,
    pub rcomp_resistor: [u16; 3],
    /// DQ ODT, DQ drive, CMD drive, CTL drive, CLK drive, in ohms.
    pub rcomp_target: [u16; 5],
    pub vtt_odt: bool,
}

/// Bounded 10-second RCOMP force handshake; preserves periodic-compensation
/// policy and all unrelated bits. Unlike the native caller, propagate failure
/// rather than logging and continuing with unknown analog compensation.
pub fn force_rcomp(r: &MchRegisters, delay: &mut impl DelayNs) -> Result<(), Error> {
    r.comp_pcu.modify(CompPcu::FORCE::SET);
    for _ in 0..10_000_000 {
        if !r.comp_pcu.is_set(CompPcu::FORCE) {
            delay.delay_us(10);
            return Ok(());
        }
        delay.delay_us(1);
    }
    Err(Error::CompensationTimeout)
}

fn round_signed(numerator: i32, denominator: i32) -> i32 {
    if numerator < 0 {
        -((-numerator + denominator / 2) / denominator)
    } else {
        (numerator + denominator / 2) / denominator
    }
}

fn vt_slope_code(slope: u16) -> u32 {
    let coding = [0i32, -125, -62, -31, 250, 125, 62, 31];
    let mut best_error = 1000;
    let mut best = 0;
    for (i, a) in coding.iter().enumerate() {
        for (j, b) in coding.iter().enumerate() {
            let error = (i32::from(slope) - (a + b)).abs();
            if error < best_error {
                best_error = error;
                best = ((i << 3) | j) as u32;
            }
        }
    }
    best
}

const RTT_NOM: [u32; 8] = [0xffff, 60, 120, 40, 240, 48, 80, 34];
fn rtt_serial(code: u8) -> u32 {
    let ohms = RTT_NOM[usize::from(code)];
    if ohms == 0xffff { ohms } else { ohms + 15 }
}
fn parallel(a: u32, b: u32) -> u32 {
    (a * b + (a + b) / 2) / (a + b)
}

/// sense_amp.c:effective_read_odt and meminit_set_default_rx_vref_ddr4,
/// table-based pre-JEDEC branch. Two-rank DIMMs take precedence when mixed.
fn initial_rx_vref(dimms: &Dimms<'_>, t: &Timings, p: &PhyPolicy) -> Result<[i8; 2], Error> {
    rx_vref(dimms, t, p, None)
}

/// sense_amp.c:effective_read_odt, including the post-JEDEC saved-MR branch.
pub(crate) fn rx_vref(
    dimms: &Dimms<'_>,
    t: &Timings,
    p: &PhyPolicy,
    saved: Option<&ModeRegisters>,
) -> Result<[i8; 2], Error> {
    rx_vref_for_clock(dimms, t.clock, p, saved)
}

pub(crate) fn rx_vref_for_clock(
    dimms: &Dimms<'_>,
    clock: super::timings::Clock,
    p: &PhyPolicy,
    saved: Option<&ModeRegisters>,
) -> Result<[i8; 2], Error> {
    if clock.tck_fs == 0 || p.vdd_mv == 0 || p.rcomp_target[0] == 0 {
        return Err(Error::InvalidPolicy);
    }
    let mut result = [0; 2];
    for (ch, channel) in dimms.iter().enumerate() {
        let ranks = channel.map(|d| d.map_or(0, |d| d.ranks));
        let have_dual = ranks.contains(&2);
        let mut sum = 0;
        let mut count = 0;
        for (slot, dimm) in channel.iter().enumerate() {
            let Some(dimm) = dimm else { continue };
            if have_dual && dimm.ranks == 1 {
                continue;
            }
            let mts = if clock.tck_fs <= 750000 {
                2667
            } else {
                clock.data_rate_mts()
            };
            let codes = |rank: usize| -> Result<(u8, u8), Error> {
                if let Some(saved) = saved {
                    let mr = saved.rank[ch][rank];
                    Ok((((mr[1] >> 8) & 7) as u8, ((mr[5] >> 6) & 7) as u8))
                } else {
                    let odt = termination(ranks, rank / 2, p.board, mts)?;
                    Ok((odt.nominal, odt.park))
                }
            };
            for local_rank in 0..usize::from(dimm.ranks) {
                let (_, park_code) = codes(slot * 2 + local_rank)?;
                let mut park = if dimm.ranks == 2 {
                    rtt_serial(park_code)
                } else {
                    0xffff
                };
                if let Some(other) = channel[slot ^ 1] {
                    let (nominal, park_code) = codes((slot ^ 1) * 2)?;
                    if other.ranks == 2 {
                        park = parallel(park, rtt_serial(park_code));
                    }
                    park = parallel(park, rtt_serial(nominal));
                }
                sum += park;
                count += 1;
            }
        }
        if count == 0 {
            continue;
        }
        let effective = sum / count;
        let target = u32::from(p.rcomp_target[0]);
        let equivalent = target * effective / (target + effective);
        let ron = 240 / 7 + 15;
        let vdd = u32::from(p.vdd_mv);
        let mv = vdd * (2 * ron + equivalent) / (ron + equivalent) / 2;
        let code = round_signed((mv as i32 * 4 - 3 * vdd as i32) * 192, (vdd * 4) as i32);
        if !(-32..=31).contains(&code) {
            return Err(Error::InvalidPolicy);
        }
        result[ch] = code as i8;
    }
    Ok(result)
}

pub(crate) fn program_rx_vref(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    ranks: [u8; 2],
    training: &mut TrainingOutput,
    vref: [i8; 2],
) {
    for ch in 0..2 {
        let ranks = ranks[ch];
        if ranks == 0 {
            continue;
        }
        // The source default-RX-Vref routine updates the eight DQ lanes.
        // ECC's ninth lane has storage, but is not calibrated by this step.
        for lane in 0..8 {
            training.ch[ch].rd_vref[lane] = vref[ch];
            for (rank, rx_train) in r.data_lane[lane][ch].rx_train.iter().enumerate() {
                if ranks & (1 << rank) == 0 {
                    continue;
                }
                rx_train.modify(RxTrain::VREF.val(vref[ch] as u32));
            }
        }
        // sense_amp.c:update_rxt: READ register-file download, never RFWR.
        for rank in 0..4 {
            if ranks & (1 << rank) == 0 {
                continue;
            }
            let ctl = &r.data_channel[ch].control0;
            ctl.modify(
                DataControl0::READ_RF_READ::SET
                    + DataControl0::READ_RF_WRITE::CLEAR
                    + DataControl0::READ_RF_RANK.val(rank),
            );
            delay.delay_us(2);
            ctl.modify(DataControl0::READ_RF_READ::CLEAR);
        }
    }
}

fn program_fub_controls(r: &MchRegisters, cpu: Cpu, t: &Timings, vdd_high: bool) {
    let ref_pi = if cpu.family == Family::KabyLake && t.clock.data_rate_mts() <= 1333 {
        7
    } else {
        0
    };
    let common = FubControl::REF_PI.val(ref_pi)
        + FubControl::DLL_MASK.val(1)
        + FubControl::VDDQ_HIGH.val(vdd_high as u32);
    let cmd_pi = RankPi::RANK0.val(64) + RankPi::RANK1.val(64);
    let rank_pi = cmd_pi + RankPi::RANK2.val(64) + RankPi::RANK3.val(64);
    for ch in 0..2 {
        r.clock_channel[ch].control.write(common);
        r.command_channel[ch]
            .control
            .write(common + FubControl::EARLY_WEAK_DRIVE.val(3) + FubControl::CMD_TX_EQ.val(1));
        r.cke_channel[ch].control.write(
            common
                + FubControl::CMD_TX_EQ.val(1)
                + FubControl::EARLY_WEAK_DRIVE.val(3)
                + FubControl::CTL_TX_EQ.val(1)
                + FubControl::CTL_SR_DRIVE.val(2),
        );
        r.control_channel[ch]
            .control
            .write(common + FubControl::CTL_TX_EQ.val(1) + FubControl::CTL_SR_DRIVE.val(2));
        r.command_channel[ch].cmd_pi.write(cmd_pi);
        r.cke_channel[ch].cmd_pi.write(cmd_pi);
        r.cke_control_channel[ch].rank_pi.write(rank_pi);
        r.clock_channel[ch].rank_pi.write(rank_pi);
        r.command_channel[ch].comp_offset.set(0);
        r.cke_control_channel[ch].comp_offset.set(0);
        r.clock_channel[ch].comp_offset.write(
            ClockCompOffset::DRIVE_UP.val(if cpu.package == Package::DesktopHalo {
                7
            } else {
                0
            }) + ClockCompOffset::DRIVE_DOWN.val(if cpu.package == Package::DesktopHalo {
                7
            } else {
                0
            }),
        );
    }
}

fn initial_slew_comp(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    t: &Timings,
    command_rate: u8,
) -> Result<(), Error> {
    let stage_ps = [59, if command_rate == 2 { 89 } else { 53 }, 53, 53];
    let min_cycle_ps = [46, 70, 70, 46];
    let cell_fields = [
        CompControl1::DQ_CELLS,
        CompControl1::CMD_CELLS,
        CompControl1::CTL_CELLS,
        CompControl1::CLK_CELLS,
    ];
    let cycle_fields = [
        CompControl1::DQ_CYCLE,
        CompControl1::CMD_CYCLE,
        CompControl1::CTL_CYCLE,
        CompControl1::CLK_CYCLE,
    ];
    let qclk_ps = t.clock.tck_fs / 2000;
    for i in 0..4 {
        let stages = (qclk_ps + stage_ps[i] / 2) / stage_ps[i];
        let phase = stages > 16 || stage_ps[i] < min_cycle_ps[i];
        let cells = if phase { stages / 2 - 1 } else { stages - 1 };
        r.comp_control1
            .modify(cell_fields[i].val(cells.min(15)) + cycle_fields[i].val(!phase as u32));
        force_rcomp(r, delay)?;
    }
    Ok(())
}

fn initial_drive_comp(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    cpu: Cpu,
    p: &PhyPolicy,
) -> Result<(), Error> {
    let mut segments = [2u32, 2, 5, 5, 3];
    if p.vtt_odt {
        segments[0] = 1;
    }
    if cpu.package == Package::DesktopHalo {
        segments[1] = 3;
    }
    let steps = [96i32, 64, 64, 64, 64];
    let resistor_index = [0, 1, 2, 2, 1];
    let fields = [
        CompControl0::DQ_ODT_VREF,
        CompControl0::DQ_DRIVE_VREF,
        CompControl0::CMD_DRIVE_VREF,
        CompControl0::CTL_DRIVE_VREF,
        CompControl0::CLK_DRIVE_VREF,
    ];
    for i in 0..5 {
        let reference = ((u32::from(p.rcomp_resistor[resistor_index[i]]) + segments[i] / 2)
            / segments[i]) as i32;
        let target = i32::from(p.rcomp_target[i]);
        let vref = round_signed(steps[i] * (reference - target), 2 * (reference + target))
            .clamp(if i == 0 { -16 } else { -8 }, if i == 0 { 15 } else { 7 });
        if i == 0 {
            r.comp_control0.modify(CompControl0::FIX_ODT_DOWN::CLEAR);
        }
        r.comp_control0.modify(fields[i].val(vref as u32));
        force_rcomp(r, delay)?;
        if i == 0 {
            let mut up = r.data_comp1.read(DataComp1::ODT_UP);
            if !(16..=48).contains(&up) {
                let disable = (up < 16) as u32;
                r.comp_control0
                    .modify(CompControl0::DISABLE_STATIC_ODT.val(disable));
                for channel in &r.data_channel {
                    channel
                        .control0
                        .modify(DataControl0::DISABLE_STATIC_ODT.val(disable));
                }
                force_rcomp(r, delay)?;
                up = r.data_comp1.read(DataComp1::ODT_UP);
            }
            let down = r.data_comp1.read(DataComp1::ODT_DOWN);
            r.comp_control0.modify(
                CompControl0::ODT_UP_DOWN.val(down.wrapping_sub(up))
                    + CompControl0::FIX_ODT_DOWN::SET,
            );
        }
    }
    Ok(())
}

/// meminit_program_mc_regs, DDR4 only. Preconditions: enabled MCHBAR,
/// resolved pre-SPD overrides (including any required reset), fused topology,
/// accepted memory PLL and initial RCOMP. Caller must stop on error. This
/// leaves periodic RCOMP disabled, scrambling off and only initial PHY values;
/// timing/MAD setup, JEDEC and ALL actual feedback training must still follow.
#[allow(clippy::too_many_arguments)]
pub fn program_mc_registers(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    mode: MemoryBootMode,
    cpu: Cpu,
    mem: &MemoryConfiguration<'_>,
    t: &Timings,
    p: &PhyPolicy,
    training: &mut TrainingOutput,
) -> Result<(), Error> {
    if mode != MemoryBootMode::Cold {
        return Err(Error::RetainedMemoryUnsupported);
    }
    // Validate divisors and analog field representability before any DDRIO
    // write; the native unsigned arithmetic must not underflow or divide by 0.
    let vdd = u32::from(p.vdd_mv);
    let vccio = u32::from(p.vccio_mv);
    let vsshi = vdd
        .checked_sub(950)
        .filter(|v| *v > 0)
        .ok_or(Error::InvalidPolicy)?;
    let qclk_ps = t.clock.tck_fs / 2000;
    if vccio <= vsshi
        || qclk_ps < 178
        || t.cas < 3
        || !(1..=2).contains(&mem.command_rate)
        || p.rcomp_resistor.contains(&0)
        || p.rcomp_target.contains(&0)
        || (mem.ecc && mem.dimms.iter().flatten().flatten().any(|d| !d.ecc_capable))
    {
        return Err(Error::InvalidPolicy);
    }
    if mem.dimms.iter().flatten().all(Option::is_none) {
        return Err(Error::NoMemory);
    }
    let vsshi_target = (vsshi * 192 / vdd)
        .checked_sub(20)
        .filter(|v| *v <= 63)
        .ok_or(Error::InvalidPolicy)?;
    let panic_threshold = (24 * 192 + vdd / 2) / vdd;
    if panic_threshold > 15 {
        return Err(Error::InvalidPolicy);
    }
    let slope = (1000u64 * 192 * u64::from(vccio) / (128 * u64::from(vdd)))
        .checked_sub(1000)
        .and_then(|v| u16::try_from(v).ok())
        .ok_or(Error::InvalidPolicy)?;
    let vref = initial_rx_vref(&mem.dimms, t, p)?;
    let ranks = [rank_mask(&mem.dimms, 0)?, rank_mask(&mem.dimms, 1)?];
    let lanes = if mem.ecc { 9 } else { 8 };
    let mts = t.clock.data_rate_mts();
    let rxbias = if mts >= 1867 {
        8
    } else if mts >= 1600 {
        10
    } else if mts >= 1333 {
        13
    } else {
        15
    };
    let misc =
        DdrMisc::DDR4::SET + DdrMisc::NO_CHANNEL_INTERLEAVE.val(!p.dq_pins_interleaved as u32);
    // Must be the first DDRIO write.
    r.ddr_misc.write(misc);
    r.mrc_revision.set(if cpu.family == Family::Skylake {
        0x02000006
    } else {
        0x03070900
    });
    for (ch, mask) in ranks.iter().copied().enumerate() {
        r.channel[ch].rank_occupancy.set(mask);
        r.clock_channel[ch]
            .ranks_used
            .set(u32::from(if p.ddp_shared_clock {
                mask & 0xd
            } else {
                mask
            }));
        r.control_channel[ch].ranks_used.set(mask.into());
        r.cke_channel[ch].ranks_used.set(mask.into());
    }
    let data = &r.data_multicast;
    let rx_default = RxTrain::RCVEN.val(64) + RxTrain::DQS_P.val(32) + RxTrain::DQS_N.val(32);
    let tx_default = TxTrain::DQ.val(96) + TxTrain::DQS.val(64) + TxTrain::EQ.val(0x3f);
    let rx_fields =
        tock_registers::LocalRegisterCopy::<u32, RxTrain::Register>::new(rx_default.value);
    let tx_fields =
        tock_registers::LocalRegisterCopy::<u32, TxTrain::Register>::new(tx_default.value);
    const PER_BIT_DEFAULT: u32 = 0x88888888;
    for rank in 0..4 {
        if (ranks[0] | ranks[1]) & (1 << rank) == 0 {
            continue;
        }
        data.rx_train[rank].write(rx_default);
        data.rx_per_bit[rank].set(PER_BIT_DEFAULT);
        data.tx_train[rank].write(tx_default);
        data.tx_per_bit[rank].set(PER_BIT_DEFAULT);
        // Publish nominal bank defaults only for real physical ranks. These
        // derive from the same words just written, never synthetic eye seeds.
        // Actual receive/WL/read/write training must replace them later.
        for ch in 0..2 {
            if ranks[ch] & (1 << rank) == 0 {
                continue;
            }
            let host = &mut training.ch[ch].rank[rank];
            host.rcven_dly[..lanes].fill(rx_fields.read(RxTrain::RCVEN) as u16);
            host.wl_dly[..lanes].fill(tx_fields.read(TxTrain::DQS) as u16);
            host.tx_dq_dly[..lanes].fill(tx_fields.read(TxTrain::DQ) as u16);
            host.rd_dqs_dly[..lanes].fill(rx_fields.read(RxTrain::DQS_P) as u8);
            host.rd_dq_dly.fill((PER_BIT_DEFAULT & 15) as u8);
            host.rx_vref_per_bit.fill((PER_BIT_DEFAULT & 15) as u8);
            host.wr_dq_dly.fill((PER_BIT_DEFAULT & 15) as u8);
        }
        for channel in &mut training.ch {
            channel.rx_dqs_p[rank][..lanes].fill(32);
            channel.rx_dqs_n[rank][..lanes].fill(32);
        }
    }
    data.tx_crosstalk.set(0);
    for offset in &data.rx_offset_vdq {
        offset.set(PER_BIT_DEFAULT);
    }
    data.offset_train.set(0);
    data.offset_comp.set(0);
    for ch in 0..2 {
        if ranks[ch] != 0 {
            training.ch[ch].data_comp_offset[..lanes].fill(0);
        }
    }
    program_rx_vref(r, delay, ranks, training, vref);
    let high = vdd > 1350;
    r.comp_control2.write(
        CompControl2::VDDQ_ODT.val(!p.vtt_odt as u32)
            + CompControl2::VTT_ODT.val(p.vtt_odt as u32)
            + CompControl2::CBEN.val(3)
            + CompControl2::CMD_DRIVE_VREF200.val(8),
    );
    data.control0
        .write(DataControl0::VDDQ_HIGH.val(high as u32));
    data.control1.write(
        DataControl1::DLL_MASK.val(1)
            + DataControl1::RX_BIAS.val(rxbias)
            + DataControl1::ODT_DELAY.val(14)
            + DataControl1::ODT_DURATION.val(7)
            + DataControl1::SENSE_AMP_DELAY.val(14)
            + DataControl1::SENSE_AMP_DURATION.val(7),
    );
    let byte_stagger = [0, 4, 1, 5, 2, 6, 3, 7, 8];
    for (ch, _) in r.data_channel.iter().enumerate() {
        for (lane, stagger) in byte_stagger.iter().enumerate().take(lanes) {
            r.data_lane[lane][ch].control2.write(
                DataControl2::RX_DQS_AMP.val(8)
                    + DataControl2::RX_CLOCK_STAGGER.val(31)
                    + DataControl2::RX_STAGGER
                        .val(((2 * u32::from(t.cas) - 6) * stagger / lanes as u32) & 0x1f)
                    + DataControl2::DDR4::SET
                    + DataControl2::VDDQ_ODT.val(!p.vtt_odt as u32)
                    + DataControl2::VTT_ODT.val(p.vtt_odt as u32)
                    + DataControl2::RX_VREF_PMFC.val(p.vtt_odt as u32)
                    + DataControl2::CBEN.val(3)
                    + DataControl2::TX_DESKEW_DISABLE.val((mts < 1600) as u32),
            );
        }
    }
    let vsshi_control = VsshiControl::GAIN_BOOST::SET
        + VsshiControl::PANIC.val(panic_threshold)
        + VsshiControl::BW_DIVIDER.val(2)
        + VsshiControl::VTT_ODT::SET
        + VsshiControl::PNC_ENABLE::SET
        + VsshiControl::ENABLE::SET
        + VsshiControl::TARGET.val(vsshi_target);
    data.vsshi_control.write(vsshi_control);
    r.comp_vsshi_control.write(vsshi_control);
    let itarget = 2500 * 20 / qclk_ps;
    let panic_up = (itarget * u32::from(p.rcomp_resistor[2]) / vsshi).min(127);
    let panic_down = (itarget * u32::from(p.rcomp_resistor[2]) / (vccio - vsshi)).min(127);
    r.comp_vsshi.write(
        VsshiComp::PANIC_VREF.val(vsshi_target)
            + VsshiComp::VT_OFFSET.val(0x1f)
            + VsshiComp::VT_SLOPE_A.val(4)
            + VsshiComp::VT_SLOPE_B.val(3)
            + VsshiComp::PANIC_UP.val(panic_up)
            + VsshiComp::PANIC_DOWN.val(panic_down),
    );
    r.data_lane[0][0].vref_control.write(
        VrefControl::OUTPUT_PANIC.val(60 * 48 / vdd)
            + VrefControl::VT_SLOPE.val(vt_slope_code(slope))
            + VrefControl::SLOPE_BOOST::SET
            + VrefControl::SLOW_BW::SET
            + VrefControl::HI_BW::SET
            + VrefControl::BW_DIVIDER.val(3),
    );
    data.vref_adjust.write(
        VrefAdjust::ENABLE_CA::SET
            + VrefAdjust::ENABLE_CH0::SET
            + VrefAdjust::ENABLE_CH1::SET
            + VrefAdjust::HIZ_TIMER.val(3)
            + VrefAdjust::VDDQ_HIGH.val(high as u32),
    );
    program_fub_controls(r, cpu, t, high);
    // mc_registers.h gives the interval shift, not a field width. Retain
    // mc_reg.c's fixed interval rather than expose an invented writable range.
    const INITIAL_COMP_INTERVAL: u32 = 10 << 1;
    r.comp_pcu
        .set(CompPcu::DISABLE::SET.value | INITIAL_COMP_INTERVAL);
    r.comp_control0.set(0);
    r.comp_control1.write(
        CompControl1::VDDQ_HIGH.val(high as u32)
            + CompControl1::VDDQ_LOW.val((vdd <= 1200) as u32)
            + CompControl1::DQ_STATIC_LEGS.val(if cpu.package == Package::DesktopHalo {
                0
            } else {
                3
            }),
    );
    initial_slew_comp(r, delay, t, mem.command_rate)?;
    initial_drive_comp(r, delay, cpu, p)?;
    r.ddr_misc.write(
        misc + DdrMisc::WEAK_LOCK_LATENCY.val(12)
            + DdrMisc::SLEEP_CYCLES.val(5)
            + DdrMisc::WAKE_CYCLES.val(2)
            + DdrMisc::DATA_CLOCK_GATE_DISABLE::SET,
    );
    for scramble in &r.scramble {
        scramble.set(0);
    }
    r.comp_override.modify(CompOverride::LS_COMP.val(6));
    data.dll_pi.write(DataDllPi::PHASE_DRIVER_POWER_SAVE::SET);
    r.cke_control_dll_pi
        .write(FubDllPi::PHASE_DRIVER_POWER_SAVE::SET);
    r.clock_dll_pi.write(FubDllPi::PHASE_DRIVER_POWER_SAVE::SET);
    r.command_dll_pi
        .write(FubDllPi::PHASE_DRIVER_POWER_SAVE::SET);
    r.data_lane[6][1].vtt_gen.write(VttGen::PANIC.val(12));
    let (top, other, vsshi_lanes) = if p.dq_pins_interleaved {
        ((2, 1), [(4, 1), (0, 1), (5, 1), (1, 1)], [(5, 0), (2, 0)])
    } else {
        ((6, 0), [(2, 1), (2, 0), (3, 1), (3, 0)], [(1, 1), (4, 0)])
    };
    r.data_lane[top.0][top.1]
        .vtt_gen
        .write(VttGen::PANIC.val(12));
    for (lane, ch) in other {
        r.data_lane[lane][ch].vtt_gen.write(VttGen::PANIC.val(8));
    }
    for (lane, ch) in vsshi_lanes {
        r.data_lane[lane][ch].vtt_gen.write(VttGen::VSSHI_COMP::SET);
    }
    r.clock_vsshi.write(ClockVsshi::VTT_COMP_FOR_VSSHI::SET);
    r.ddr_voltage.set(match vdd {
        1350 => 1,
        1200 => 7,
        _ => 0,
    });
    force_rcomp(r, delay)
}

#[cfg(test)]
mod tests {
    use super::super::{
        spd::{
            Dimm,
            testing::{seal, sodimm},
        },
        timings,
    };
    use super::*;

    fn policy() -> PhyPolicy {
        PhyPolicy {
            board: BoardType::Desktop,
            dq_pins_interleaved: true,
            ddp_shared_clock: false,
            vdd_mv: 1200,
            vccio_mv: 950,
            rcomp_resistor: [200, 81, 162],
            rcomp_target: [100, 40, 40, 23, 40],
            vtt_odt: false,
        }
    }

    #[test]
    fn sense_amp_initial_rx_vref_termination_topologies_and_vt_slope_coding() {
        let raw = sodimm();
        let mut dual = raw;
        dual[12] = 9;
        seal(&mut dual);
        let single = Dimm::decode(&raw).unwrap();
        let dual = Dimm::decode(&dual).unwrap();
        let t = timings::decode(&[[Some(single), None], [None; 2]], 938000).unwrap();
        let p = policy();
        assert_eq!(
            initial_rx_vref(&[[Some(single), None], [None; 2]], &t, &p).unwrap(),
            [-16, 0]
        );
        let mixed = initial_rx_vref(
            &[[Some(single), Some(dual)], [Some(dual), Some(single)]],
            &t,
            &p,
        )
        .unwrap();
        assert_eq!(mixed, [11, 13]);
        assert_eq!(vt_slope_code(0), 0);
        assert_eq!(vt_slope_code(500), 36);
        assert_eq!(round_signed(-25, 10), -3);
        assert_eq!(round_signed(25, 10), 3);
    }
}
