// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native post_training.c DDR4 activation and training.c:meminit_mc_done.
//! Callable cold-only phases, not a dispatcher, cache restore or RAM handoff.
//! DRAM thermal controls here are NOT an M900 fan policy.
use super::{
    Error,
    additional_steps::OltmPolicy,
    cpu::Package,
    frequency::qclk_ps,
    memory_check::stopped,
    memory_map::ProgrammedMap,
    phy_registers::{DataControl1, DataControl2, DdrMisc},
    registers::{InitState, ReutSequence, Scheduler},
    reut::Engine,
    sa_registers::*,
    timings::Clock,
    training::cold_ranks,
    training_state::CalibrationState,
    uncore_prmrr,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_pci::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerDown {
    Disabled,
    Active,
    PrechargeDllOff,
    Auto,
}
impl PowerDown {
    fn image(self, package: Package) -> u32 {
        let mobile = package == Package::UltUlx;
        (if mobile { 0x40 } else { 0x80 })
            | match self {
                Self::Disabled => 0,
                Self::Active => 1 << 12,
                Self::PrechargeDllOff => (1 << 13) | (1 << 14),
                Self::Auto => (1 << 13) | if mobile { 1 << 14 } else { 0 },
            }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ActivationPolicy {
    pub package: Package,
    pub power_down: PowerDown,
    pub thermal: OltmPolicy,
    pub ecc: bool,
    pub sa_gv: bool,
    /// Native TE_TAT. When set the actual preceding turnaround step must have
    /// applied sample-ODT guardband 1. Otherwise activation computes it here.
    pub turnaround_trained: bool,
    /// Caller-sampled per-channel scrambler seeds. Native mc_activate uses
    /// TSC low bits XOR (channel << 8); zero is valid, not a crypto guarantee.
    pub scrambling_keys: [u16; 2],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationError {
    Preflight(Error),
    SelfRefreshTimeout,
    MapMismatch(u16),
    Readback(u16),
    /// MC locks may already be irreversible. No rollback is attempted.
    PartialPrmrr(uncore_prmrr::Error),
    /// Fatal after MC/PRMRR/map/PTM locks, never retrain-success.
    PartialAckTimeout,
}
impl From<Error> for ActivationError {
    fn from(e: Error) -> Self {
        Self::Preflight(e)
    }
}

impl Engine<'_> {
    /// Source DDR4 RH prevention is genuinely a no-op, not fixed LFSR seeds.
    pub fn meminit_post_training(
        &self,
        mode: MemoryBootMode,
        package: Package,
        power_down: PowerDown,
    ) -> Result<(), Error> {
        cold_ranks(self, mode)?;
        stopped(self)?;
        if self.registers.mc_lock.get() != 0 {
            return Err(Error::InvalidPolicy);
        }
        self.registers
            .power_down_config
            .set(power_down.image(package));
        Ok(())
    }
    /// # Safety
    /// Exclusive cold controller after genuine mandatory training, before
    /// alias/full-memory tests and the final MC handoff. Production seeds must
    /// originate from the native per-channel TSC sample, not fabricated state.
    /// No SA-GV, retained boot, ECC, trace mode or user power-weight profile.
    /// A post-write error is fatal partial activation, not a rollback promise.
    pub unsafe fn meminit_mc_activate(
        &self,
        s: &CalibrationState,
        mode: MemoryBootMode,
        clock: Clock,
        policy: ActivationPolicy,
        delay: &mut impl DelayNs,
    ) -> Result<(), ActivationError> {
        let ranks = cold_ranks(self, mode)?;
        stopped(self)?;
        let qclk = qclk_ps(self.registers, clock)?;
        if policy.sa_gv
            || policy.ecc
            || policy.thermal.user_power_weights
            || policy.thermal.refresh_2x > 3
            || self.registers.mc_lock.get() != 0
            || self.registers.ddr_ptm.get() & (1 << 5) != 0
            || self.registers.init_state.get()
                & ((1 << 0) | (1 << 5) | (1 << 7) | (1 << 11) | (1 << 12))
                != 0
            || self
                .registers
                .mad_intra
                .iter()
                .any(|r| r.get() & (3 << 12) != 0)
        {
            return Err(Error::InvalidPolicy.into());
        }
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            if self.registers.scramble[ch].get() != 0 {
                return Err(Error::InvalidPolicy.into());
            }
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                for b in 0..8 {
                    if s.training.ch[ch].rx_dqs_p[rank][b] > 63
                        || s.training.ch[ch].rx_dqs_n[rank][b] > 63
                        || s.training.ch[ch].rank[rank].rcven_dly[b] > 511
                    {
                        return Err(Error::InvalidTiming.into());
                    }
                }
            }
        }
        self.registers
            .ddr_misc
            .modify(DdrMisc::DATA_CLOCK_GATE_DISABLE::CLEAR);
        for ch in 0..2 {
            if ranks[ch] != 0 {
                self.registers.scramble[ch].set(
                    1 | (u32::from(policy.scrambling_keys[ch]) << 1)
                        | (3 << 17)
                        | (3 << 19)
                        | (1 << 21),
                );
            }
            self.registers.channel[ch]
                .scheduler
                .modify(Scheduler::COMMAND_THREE_STATE::CLEAR);
            self.registers.reut_sequence[ch][0].write(ReutSequence::INIT_MODE::Normal);
            self.registers.reut_sequence[ch][1].set(0);
        }
        // Source ten-second bound. Observe actual SR state; never force clear.
        let mut out = false;
        for _ in 0..10_000_000 {
            if self.registers.stall_drain.get() & (1 << 8) == 0 {
                out = true;
                break;
            }
            delay.delay_us(1);
        }
        if !out {
            return Err(ActivationError::SelfRefreshTimeout);
        }
        self.registers
            .power_down_config
            .set(policy.power_down.image(policy.package));
        self.activate_tail(s, ranks, qclk, policy.turnaround_trained);
        self.registers.self_refresh_config.set((1 << 16) | 0x200);
        delay.delay_us(1);
        self.thermal_overrides(policy.thermal);
        self.power_weights(ranks);
        Ok(())
    }
    /// warm_reset.c:meminit_normal_mode() for a fast boot with SA GV off.
    /// The cached registers already hold mc_activate's scrambler, scheduler,
    /// data-lane and thermal images; only the REUT sequencer mode, the
    /// self-refresh exit and the shared power-down register remain.
    pub fn meminit_normal_mode_fast(
        &self,
        mode: MemoryBootMode,
        package: Package,
        power_down: PowerDown,
        delay: &mut impl DelayNs,
    ) -> Result<(), ActivationError> {
        cold_ranks(self, mode)?;
        stopped(self)?;
        // Native writes both channels, populated or not.
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0].write(ReutSequence::INIT_MODE::Normal);
            self.registers.reut_sequence[ch][1].set(0);
        }
        let mut out = false;
        for _ in 0..1_000_000 {
            if self.registers.stall_drain.get() & (1 << 8) == 0 {
                out = true;
                break;
            }
            delay.delay_us(10);
        }
        if !out {
            return Err(ActivationError::SelfRefreshTimeout);
        }
        self.registers
            .power_down_config
            .set(power_down.image(package));
        Ok(())
    }
    fn activate_tail(
        &self,
        s: &CalibrationState,
        ranks: [u8; 2],
        qclk: u32,
        turnaround_trained: bool,
    ) {
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            for b in 0..8 {
                let mut min_rcven = 512u16;
                let mut max_rcven = 0u16;
                let mut max_pi = 0u8;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    let r = &s.training.ch[ch];
                    min_rcven = min_rcven.min(r.rank[rank].rcven_dly[b]);
                    max_rcven = max_rcven.max(r.rank[rank].rcven_dly[b]);
                    max_pi = max_pi.max(r.rx_dqs_p[rank][b]).max(r.rx_dqs_n[rank][b]);
                }
                let lane = &self.registers.data_lane[b][ch];
                if !turnaround_trained {
                    let wakeup = (80_000 / qclk).max(128) as i32;
                    let on = (((i32::from(min_rcven) - wakeup) >> 6) - 1).clamp(-4, 6);
                    let off = ((i32::from(max_rcven) >> 6) + 1 - on).clamp(0, 7);
                    lane.control1.modify(
                        DataControl1::ODT_DELAY.val(on as u32 & 15)
                            + DataControl1::SENSE_AMP_DELAY.val(on as u32 & 15)
                            + DataControl1::ODT_DURATION.val(off as u32)
                            + DataControl1::SENSE_AMP_DURATION.val(off as u32),
                    );
                }
                // Source SDLL segment mask is 3 bits [9:7], despite its
                // [10:7] comment. Preserve the independent RX-bias bit10.
                lane.control1.set(
                    (lane.control1.get() & !(7 << 7)) | (u32::from((7 - (max_pi >> 3)) & !1) << 7),
                );
                let c1 = lane.control1.extract();
                let raw = c1.read(DataControl1::ODT_DELAY) as i32;
                let signed = if raw & 8 != 0 { raw - 16 } else { raw };
                let odt_off =
                    (signed + c1.read(DataControl1::ODT_DURATION) as i32 + 14).clamp(0, 31) as u32;
                let stage = if self.registers.data_lane[0][ch].control0.get() & (1 << 30) != 0 {
                    let max_channel = (0..4)
                        .filter(|r| ranks[ch] & (1 << r) != 0)
                        .flat_map(|r| s.training.ch[ch].rank[r].rcven_dly[..8].iter().copied())
                        .max()
                        .unwrap_or(0);
                    let long =
                        u32::from(self.registers.data_lane[0][ch].control1.get() & (1 << 31) != 0);
                    lane.control2
                        .read(DataControl2::RX_CLOCK_STAGGER)
                        .max((u32::from(max_channel) / 64 + 17 + long).min(31))
                } else {
                    17.max(odt_off)
                };
                lane.control2
                    .modify(DataControl2::RX_CLOCK_STAGGER.val(stage));
            }
        }
    }
    fn thermal_overrides(&self, p: OltmPolicy) {
        for r in &self.registers.rapl_limit {
            r.set(0);
        }
        for r in &self.registers.thermal_threshold {
            r.set(0xffff);
        }
        for r in &self.registers.thermal_budget {
            r.set(0xffff);
        }
        for c in &self.registers.channel {
            c.channel_power_down
                .set((c.channel_power_down.get() & !(0x1ff << 16)) | (0x30 << 16));
        }
        self.registers.self_refresh_config.set((1 << 16) | 0x200);
        let sensitive = self
            .dimms
            .iter()
            .flatten()
            .flatten()
            .any(|d| d.tcr_sensitive_hynix || d.tcr_sensitive_micron);
        let bits = u32::from(p.enable_oltm)
            | (u32::from(p.enable_cltm) << 1)
            | (u32::from(p.refresh_2x) << 2)
            | (1 << 6)
            | (u32::from(!p.ddr_thermal_sensor) << 7)
            | (u32::from(!sensitive) << 8);
        let mask = 3 | (3 << 2) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 8);
        self.registers
            .ddr_ptm
            .set((self.registers.ddr_ptm.get() & !mask) | bits);
    }
    fn power_weights(&self, ranks: [u8; 2]) {
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            let mask = (0..2)
                .filter(|d| self.dimms[ch][*d].is_some())
                .fold(0, |v, d| v | 0xff << (8 * d));
            for (r, value) in self.registers.channel[ch]
                .energy
                .iter()
                .zip([0x0b, 0x07, 0x8f, 0xc8, 0x96])
            {
                r.set((value * 0x0101_0101) & mask);
            }
        }
        self.registers.channel_power_floor.set(0);
        self.registers.energy_scale.set(5);
    }
    /// # Safety
    /// Exclusive final cold handoff AFTER activation and mandatory destructive
    /// validation. `map` must be the actual unchanged programmed map; `cpu` is
    /// the actual package MSR endpoint. No SMRAM enable, DID, MP/SMM or usable
    /// memory certificate is implied. Every error after the first write is
    /// fatal partial configuration; locks are never rolled back.
    pub unsafe fn meminit_mc_done(
        &self,
        mode: MemoryBootMode,
        sa_gv: bool,
        sa: &EcamDevice,
        map: &ProgrammedMap,
        cpu: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<(), ActivationError> {
        let ranks = cold_ranks(self, mode)?;
        stopped(self)?;
        if sa_gv
            || self.registers.mc_lock.get() != 0
            || self.registers.ddr_ptm.get() & (1 << 5) != 0
            || self.registers.stall_drain.get() & (1 << 8) != 0
            || self.registers.init_state.get() & ((1 << 0) | (1 << 5) | (1 << 7)) != 0
            || (0..2).any(|ch| {
                self.registers.reut_sequence[ch][0].read(ReutSequence::INIT_MODE) != 3
                    // SCRAM_EN is required; a zero TSC-derived seed is valid.
                    || (ranks[ch] != 0 && self.registers.scramble[ch].get() & 1 == 0)
            })
        {
            return Err(Error::InvalidPolicy.into());
        }
        super::early_sa::validate_host(sa)
            .map_err(|_| ActivationError::Preflight(Error::HostBridgeUnavailable))?;
        let p = map.plan();
        for (offset, value, wide) in [
            (TOM, p.tom, true),
            (TOLUD, p.tolud, false),
            (TOUUD, p.touud, true),
            (REMAP_BASE, p.remap_base, true),
            (REMAP_LIMIT, p.remap_limit, true),
            (BDSM, p.bdsm, false),
            (BGSM, p.bgsm, false),
            (TSEG, p.tseg, false),
        ] {
            let live = u64::from(sa.read32(offset))
                | if wide {
                    u64::from(sa.read32(offset + 4)) << 32
                } else {
                    0
                };
            if live & !1 != value {
                return Err(ActivationError::MapMismatch(offset));
            }
        }
        if sa.read32(DPR) & !0xfff0_0001 != 0 {
            return Err(ActivationError::MapMismatch(DPR));
        }
        let mebase = u64::from(sa.read32(MESEG_BASE)) | u64::from(sa.read32(MESEG_BASE + 4)) << 32;
        if mebase != p.me_physical_base.unwrap_or(0) {
            return Err(ActivationError::MapMismatch(MESEG_BASE));
        }
        let me_mask = u64::from(sa.read32(MESEG_MASK)) | u64::from(sa.read32(MESEG_MASK + 4)) << 32;
        let wanted_me = if p.me_size_mib == 0 {
            0
        } else {
            ((u64::from(p.me_size_mib).wrapping_neg() & 0x7ffff) << 20) | (1 << 11)
        };
        if me_mask & !(1 << 10) != wanted_me {
            return Err(ActivationError::MapMismatch(MESEG_MASK));
        }
        let ggc = sa.read16(GGC);
        let gms = u64::from(ggc >> 8);
        let gtt = [0u64, 2, 4, 8][((ggc >> 6) & 3) as usize];
        if ggc & 0x38 != 0
            || gms > 16
            || p.tolud - p.bdsm != gms * 32 * (1 << 20)
            || p.bdsm - p.bgsm != gtt * (1 << 20)
            || (ggc & 2 == 0 && (gms == 0 || sa.read32(DEVEN) & (1 << 4) == 0))
        {
            return Err(ActivationError::MapMismatch(GGC));
        }
        self.registers.init_state.modify(
            InitState::REFRESH_ENABLE::SET + InitState::MRC_DONE::SET + InitState::MRC_DONE2::SET,
        );
        self.registers.mc_lock.set(0x8f);
        if self.registers.mc_lock.get() != 0x8f {
            return Err(ActivationError::Readback(0x50fc));
        }
        uncore_prmrr::lock(cpu, map.uncore_prmrr()).map_err(ActivationError::PartialPrmrr)?;
        for offset in [TOM, TOLUD, TOUUD, REMAP_BASE, REMAP_LIMIT, DPR, BDSM, BGSM] {
            let live = sa.read32(offset);
            if live & 1 == 0 {
                sa.write32(offset, live | 1);
            }
            if sa.read32(offset) != live | 1 {
                return Err(ActivationError::Readback(offset));
            }
        }
        let me = sa.read32(MESEG_MASK);
        if me & (1 << 10) == 0 {
            sa.write32(MESEG_MASK, me | (1 << 10));
        }
        if sa.read32(MESEG_MASK) != me | (1 << 10) {
            return Err(ActivationError::Readback(MESEG_MASK));
        }
        let ggc = sa.read16(GGC);
        if ggc & 1 == 0 {
            sa.write16(GGC, ggc | 1);
        }
        if sa.read16(GGC) != ggc | 1 {
            return Err(ActivationError::Readback(GGC));
        }
        self.registers
            .ddr_ptm
            .set(self.registers.ddr_ptm.get() | (1 << 5));
        if self.registers.ddr_ptm.get() & (1 << 5) == 0 {
            return Err(ActivationError::Readback(0x5880));
        }
        for _ in 0..10_000_000 {
            if self.registers.init_state.is_set(InitState::DONE_ACK) {
                self.registers.comp_pcu.set(0x0a << 1);
                self.registers.scratchpad.set(1);
                return Ok(());
            }
            delay.delay_us(1);
        }
        Err(ActivationError::PartialAckTimeout)
    }
}
