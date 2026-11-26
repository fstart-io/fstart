// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 timing/refresh/scheduler programming from native/mc_timing.c.
//! Standard profile, SA-GV off. This is initial 3N training configuration;
//! later training must update command rate and turnarounds before normal mode.

use super::{
    Error,
    cpu::{Cpu, Package},
    registers::*,
    spd::Dimms,
    timings::Timings,
    training_state::CalibrationState,
};
use tock_registers::interfaces::{ReadWriteable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelTiming {
    pre: u32,
    act: u32,
    power_down: u32,
    pub rdrd: u32,
    pub rdwr: u32,
    pub wrrd: u32,
    pub wrwr: u32,
    pub odt: u32,
    pub scheduler: u32,
    write_delay: u32,
    bank_map: u64,
    mr2_shadow: Option<u32>,
    refresh_policy: u32,
    refresh_timing: u32,
    self_refresh_timing: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingRegisters {
    pub channel: [ChannelTiming; 2],
    dram_odt: bool,
}

fn tcke(mts: u32) -> u32 {
    match mts {
        ..=1067 => 3,
        1068..=1600 => 4,
        1601..=1867 => 5,
        1868..=2400 => 6,
        2401..=2800 => 7,
        _ => 8,
    }
}
fn tccd_l(mts: u32) -> u32 {
    match mts {
        ..=1333 => 4,
        1334..=1867 => 5,
        1868..=2400 => 6,
        _ => 7,
    }
}
pub(crate) fn txp(mts: u32) -> u32 {
    match mts {
        ..=1600 => 5,
        1601..=1867 => 6,
        1868..=2133 => 7,
        _ => 8,
    }
}
fn txsdll(mts: u32) -> u32 {
    match mts {
        ..=1867 => 597,
        1868..=2400 => 768,
        _ => 1024,
    }
}

impl TimingRegisters {
    pub fn for_dimms(
        t: &Timings,
        dimms: &Dimms<'_>,
        cpu: Cpu,
        dram_odt: bool,
        saved_mr2: [[u16; 4]; 2],
    ) -> Result<Self, Error> {
        let cl = u32::from(t.cas);
        let cwl = u32::from(t.cwl);
        // Source unsigned turnaround expressions must not wrap. Timings that
        // cannot be represented by this initial MC configuration fail closed.
        // Refresh recovery must never be shortened to fit the hardware.
        if cl < cwl || cl > cwl + 7 || t.trfc > 0x3ff {
            return Err(Error::InvalidTiming);
        }
        let tck_fs = 1_000_000_000_000u64 / u64::from(t.clock.clock_khz());
        let tck_fs = tck_fs as u32;
        let tck_ps = tck_fs / 1000;
        let mts = t.clock.data_rate_mts();
        let tcwl = (cwl - 1).clamp(4, 20);
        let pre = (TcPre::TRP.val(u32::from(t.trp).clamp(8, 63))
            + TcPre::TRAS.val(u32::from(t.tras).clamp(28, 64))
            + TcPre::TRTP.val(u32::from(t.trtp).clamp(6, 15))
            + TcPre::TWRPRE.val((4 + u32::from(t.twr) + cwl).clamp(23, 96)))
        .value;
        let act = (TcAct::TFAW.val(u32::from(t.tfaw).clamp(16, 63))
            + TcAct::TRRD_L.val(u32::from(t.trrd_l).clamp(4, 15))
            + TcAct::TRRD_S.val(u32::from(t.trrd_s).clamp(4, 15))
            + TcAct::DERATING.val(2))
        .value;
        let odt = (TcOdt::READ_DELAY.val((cl - cwl).min(7))
            + TcOdt::WRITE_DURATION
                .val((cpu.package == Package::DesktopHalo && cpu.model() != 0x506e0) as u32)
            + TcOdt::TCL.val(cl.clamp(5, 31))
            + TcOdt::TCWL.val(tcwl)
            + TcOdt::TAONPD.val(8500u32.div_ceil(tck_ps).min(31)))
        .value;
        let power_down = (TcPowerDown::TCKE.val(tcke(mts).clamp(4,15))
            +TcPowerDown::TXP.val(txp(mts).clamp(5,15))
            +TcPowerDown::TXPDLL.val(24_000_000u32.div_ceil(tck_fs).clamp(4,63))
            // Use the PLL speed bin, not clock_khz/500 truncation at 2133.
            +TcPowerDown::TPRPDEN.val(if t.clock.tck_fs<=938000 {2} else {1})
            +TcPowerDown::TRDPDEN.val((cl+5).clamp(4,95))
            +TcPowerDown::TWRPDEN.val((u32::from(t.twr)+cwl+4).clamp(4,95)))
        .value;
        let rdrd = (Turnaround::SG.val(tccd_l(mts).min(54))
            + Turnaround::DG.val(4)
            + Turnaround::DR.val(6)
            + Turnaround::DD.val(7))
        .value;
        let rdwr_value = cl - cwl + 7 + u32::from(mts > 1067) + u32::from(mts > 1600);
        let rdwr = (Turnaround::SG.val(rdwr_value.min(54))
            + Turnaround::DG.val(rdwr_value.min(54))
            + Turnaround::DR.val(rdwr_value.min(54))
            + Turnaround::DD.val((rdwr_value + 1).min(54)))
        .value;
        let wrrd_dr = (cwl + 7 - cl).clamp(4, 54);
        let wrrd = (Turnaround::SG.val((cwl + 6 + u32::from(t.twtr_l)).min(86))
            + Turnaround::DG.val((cwl + 6 + u32::from(t.twtr_s)).min(54))
            + Turnaround::DR.val(wrrd_dr)
            + Turnaround::DD.val(wrrd_dr))
        .value;
        let wrwr = (Turnaround::SG.val(tccd_l(mts).min(54))
            + Turnaround::DG.val(4)
            + Turnaround::DR.val(7)
            + Turnaround::DD.val(7))
        .value;
        let d = u32::from(tcwl >= 7);
        let write_delay = (WriteDelay::RANK0.val(d + 1)
            + WriteDelay::RANK1.val(d + 1)
            + WriteDelay::RANK2.val(d + 1)
            + WriteDelay::RANK3.val(d + 1)
            + WriteDelay::DEC_WRD.val(d))
        .value;
        let trefi = u32::from(t.trefi);
        let refresh_policy = (RefreshPolicy::LOW_WATERMARK.val(0x40)
            + RefreshPolicy::HIGH_WATERMARK.val(8)
            + RefreshPolicy::PANIC_WATERMARK.val(9)
            + RefreshPolicy::TREFI_X9.val((trefi * 89 / (1024 * 10)).min(0x7f)))
        .value;
        let refresh_timing = (RefreshTiming::TREFI.val(trefi.min(0xffff))
            + RefreshTiming::TRFC.val(u32::from(t.trfc)))
        .value;
        let self_refresh_timing = (SelfRefreshTiming::TXSDLL.val(txsdll(mts))
            + SelfRefreshTiming::TXS_OFFSET
                .val(10_000_000u32.div_ceil(tck_fs).saturating_sub(8).min(15))
            + SelfRefreshTiming::TZQOPER.val(512)
            + SelfRefreshTiming::TMOD.val(24))
        .value;
        let base = ChannelTiming {
            pre,
            act,
            power_down,
            rdrd,
            rdwr,
            wrrd,
            wrwr,
            odt,
            write_delay,
            scheduler: 0,
            bank_map: 0,
            mr2_shadow: None,
            refresh_policy,
            refresh_timing,
            self_refresh_timing,
        };
        let mut result = Self {
            channel: [base; 2],
            dram_odt,
        };
        for (ch, channel) in dimms.iter().enumerate() {
            let mut mirror = 0;
            let mut x8 = 0;
            let mut min_banks = 16;
            let mut mr2_shadow = 0;
            let mut have_mr2 = false;
            for (slot, dimm) in channel.iter().enumerate() {
                let Some(dimm) = dimm else {
                    continue;
                };
                mirror |= u32::from(dimm.rank_one_mirrored) << slot;
                x8 |= u32::from(dimm.device_width == 8) << slot;
                min_banks = min_banks.min(u32::from(dimm.bank_groups) * u32::from(dimm.banks));
                let mr2 = u32::from(saved_mr2[ch][slot * 2]);
                if mr2 != 0 {
                    have_mr2 = true;
                    // MR2 is per channel; SRT availability remains per DIMM.
                    mr2_shadow = (mr2_shadow & !0x3fff) | (mr2 & 0x3fff);
                    if mr2 & (1 << 7) != 0 && mr2 & (1 << 6) == 0 {
                        mr2_shadow |= 1 << (14 + slot);
                    }
                }
            }
            result.channel[ch].scheduler = (Scheduler::TECHNOLOGY.val(0)
                + Scheduler::COMMAND_STRETCH.val(2)
                + Scheduler::N_TO_ONE.val(3)
                + Scheduler::ADDRESS_MIRROR.val(mirror)
                + Scheduler::X8_DEVICE.val(x8)
                + Scheduler::CPDED.val(4)
                + Scheduler::COMMAND_THREE_STATE::SET
                + Scheduler::CLOCK_TO_CKE.val(4))
            .value;
            result.channel[ch].bank_map = if min_banks == 16 {
                0xfedcba9876543210
            } else {
                0x7654321076543210
            };
            if have_mr2 {
                result.channel[ch].mr2_shadow = Some(mr2_shadow);
            }
        }
        Ok(result)
    }

    /// PLL/RCOMP and PHY register programming must precede this, with MAD
    /// setup and forced RCOMP following. Does not activate normal MC mode.
    /// Program initial timing words AND their pre-RCOMP host baselines in
    /// one operation. No MC timing/scheduler readback is safe before RCOMP.
    /// These are initial 3N defaults, not measured/final timing or usable RAM.
    pub fn program(&self, registers: &MchRegisters, state: &mut CalibrationState) {
        for (ch, t) in self.channel.iter().enumerate() {
            let r = &registers.channel[ch];
            if let Some(mr2) = t.mr2_shadow {
                r.mr2_shadow.set(mr2);
            }
            r.tc_pre.set(t.pre);
            r.tc_act.set(t.act);
            r.tc_power_down.set(t.power_down);
            r.tc_rdrd.set(t.rdrd);
            r.tc_rdwr.set(t.rdwr);
            r.tc_wrrd.set(t.wrrd);
            r.tc_wrwr.set(t.wrwr);
            r.tc_odt.set(t.odt);
            r.write_delay.set(t.write_delay);
            r.scheduler.set(t.scheduler);
            state.mchbar_tc_rdrd[ch] = t.rdrd;
            state.mchbar_tc_rdwr[ch] = t.rdwr;
            state.mchbar_tc_odt[ch] = t.odt;
            state.mchbar_sc_gs_cfg[ch] = t.scheduler;
            registers.reut_bank_map[ch][0].set(t.bank_map as u32);
            registers.reut_bank_map[ch][1].set((t.bank_map >> 32) as u32);
            r.refresh_policy.set(t.refresh_policy);
            r.refresh_timing.set(t.refresh_timing);
            r.self_refresh_timing.set(t.self_refresh_timing);
            r.zq_calibration
                .write(ZqCalibration::PERIOD.val(128) + ZqCalibration::TZQCS.val(128));
            r.refresh_stagger.write(RefreshStagger::DISPLAY_ENABLE::SET);
        }
        registers.decode_cbit.modify(
            DecodeCbit::DIS_SINGLE_CHANNEL_SR::CLEAR
                + DecodeCbit::DIS_OTHER_CHANNEL_STOLEN_REFRESH::CLEAR,
        );
        registers.decode_misc.modify(DecodeMisc::SPARE.val(0x13));
        registers
            .driver_gate
            .modify(DriverGate::BIT10::SET + DriverGate::BIT13::SET);
        registers
            .sched_cbit
            .write(SchedulerCbit::DIS_ODT.val(!self.dram_odt as u32));
        let wdb = (WdbWatermarks::LOW.val(0x38)
            + WdbWatermarks::HIGH.val(0x28)
            + WdbWatermarks::PENDING.val(0x3f)
            + WdbWatermarks::WRITE_CAS.val(0x38)
            + WdbWatermarks::READ_CAS.val(0x10)
            + WdbWatermarks::READ_VC1.val(0x10)
            + WdbWatermarks::WRITE_STARVATION.val(0x40))
        .value;
        registers.sched_wdbwm[0].set(wdb as u32);
        registers.sched_wdbwm[1].set((wdb >> 32) as u32);
        let qos = (Qos::ISO_WINDOW.val(0x1c2)
            + Qos::WRITE_STARVATION_WINDOW.val(0x5a)
            + Qos::READ_STARVATION_WINDOW.val(0x5a)
            + Qos::VC0_READ_COUNT.val(0x16)
            + Qos::VC1_READ_STARVATION::SET
            + Qos::WRITE_STARVATION_ISO::SET
            + Qos::READ_STARVATION_ISO::SET
            + Qos::DIS_VC0_COUNTER::SET)
            .value;
        registers.sched_qos[0].set(qos as u32);
        registers.sched_qos[1].set((qos >> 32) as u32);
        registers
            .sched_second_cbit
            .write(SchedulerSecondCbit::DIS_BURST_REFRESH_LIMIT::SET);
        registers
            .sched_pcit
            .write(Pcit::CPU.val(0x40) + Pcit::GT.val(0x60));
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        registers::testing::image,
        spd::{
            Dimm,
            testing::{seal, sodimm},
        },
        timings,
    };
    use super::*;
    use tock_registers::interfaces::Readable;
    #[test]
    fn mc_timing_full_width_spd_refresh_and_unrepresentable_recovery() {
        let mut raw = sodimm();
        raw[4] = 0x86;
        raw[5] = 0x29;
        for (offset, value) in [(30, 4400u16), (32, 4200), (34, 4100)] {
            raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(d), None], [None; 2]];
        let mut t = timings::decode(&dimms, 938000).unwrap();
        assert_eq!((t.trfc, t.trfc2, t.trfc4), (587, 560, 547));
        let cpu = Cpu::from_signature(0x906e9).unwrap();
        let p = TimingRegisters::for_dimms(&t, &dimms, cpu, true, [[0; 4]; 2]).unwrap();
        let image = image();
        p.program(image.registers(), &mut CalibrationState::default());
        assert_eq!(
            image.registers().channel[0]
                .refresh_timing
                .read(RefreshTiming::TRFC),
            587
        );
        t.trfc = 1023;
        assert!(TimingRegisters::for_dimms(&t, &dimms, cpu, true, [[0; 4]; 2]).is_ok());
        t.trfc = 1024;
        assert_eq!(
            TimingRegisters::for_dimms(&t, &dimms, cpu, true, [[0; 4]; 2]),
            Err(Error::InvalidTiming)
        );
    }
    #[test]
    fn mc_timing_x16_bank_map_and_mr2_shadow_srt_and_skl_odt() {
        let mut raw = sodimm();
        raw[4] = 0x45;
        raw[5] = 0x21;
        raw[12] = 2;
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(d), None], [None, None]];
        let t = timings::decode(&dimms, 938000).unwrap();
        let p = TimingRegisters::for_dimms(
            &t,
            &dimms,
            Cpu::from_signature(0x506e3).unwrap(),
            false,
            [[0x80, 0, 0, 0], [0; 4]],
        )
        .unwrap();
        let image = image();
        p.program(image.registers(), &mut CalibrationState::default());
        let r = image.registers();
        assert_eq!(r.reut_bank_map[0][1].get(), 0x76543210);
        assert_eq!(r.channel[0].mr2_shadow.get(), 0x4080);
        assert_eq!(r.channel[0].tc_odt.read(TcOdt::WRITE_DURATION), 0);
        assert_eq!(r.sched_cbit.get(), 1 << 12);
    }
}
