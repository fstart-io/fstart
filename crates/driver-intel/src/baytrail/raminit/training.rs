//! DDR3 training (coreboot `romstage/training.c`): receive enable, fine and
//! coarse write leveling, and the read and write eye centering, on channel
//! 0 through the shared delay elements.

use super::cpgc::{Cpgc, Test};
use super::ddrio_regs::{
    CLKCTL_DDR3RESETCTL, CMPCTRL, DQ_B0RXIOBUFCTL, DQ_NUM_MODULES, DQ_OVRCTL_B0, DQ_OVRCTL_B1,
    clkctl_ddr3resetctl, cmpctrl, dq_b0rxiobufctl, dq_b1rxiobufctl, dq_ovrctl_b0, dq_ovrctl_b1,
};
use super::delay::{
    self, Delay, Element, FIFO_RST, FULL_CLK, HALF_CLK, QTR_CLK, RCVN_MIN, RDQS_PI, WDQ_MIN,
    WDQS_MIN,
};
use super::dunit::{DRMC, DRMC_REG, JEDEC, MR1};
use super::{NUM_RANKS, Params, STROBES};
use crate::baytrail::iosf::{DUNIT_CH0, Iosf, OP_DRAM_INIT};
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::ReadWriteable;

const NUM_SAMPLES: usize = 3;
const MAX_LINEAR_DELAY: i32 = 7 * FULL_CLK;

type Strobes<T> = [T; STROBES];

struct Trainer<'a, P> {
    iosf: &'a Iosf<P>,
    cpgc: Cpgc<'a, P>,
}

impl<'a, P: ConfigRegionAccess> Trainer<'a, P> {
    fn new(iosf: &'a Iosf<P>) -> Self {
        Self {
            iosf,
            cpgc: Cpgc::new(iosf),
        }
    }

    fn write(&self, e: Element, strobe: usize, value: u32) {
        delay::write(self.iosf, e, 0, 0, strobe, value);
    }

    /// `e` in every rank: the minimum selects are per rank.
    fn write_all_ranks(&self, e: Element, strobe: usize, value: u32) {
        for rank in 0..NUM_RANKS {
            delay::write(self.iosf, e, 0, rank, strobe, value);
        }
    }

    fn delay(&self, d: Delay, strobe: usize) -> i32 {
        delay::get(self.iosf, d, 0, strobe)
    }

    fn set_delay(&self, d: Delay, strobe: usize, value: i32) {
        delay::set(self.iosf, d, 0, strobe, value);
    }

    fn wclk(&self) -> i32 {
        delay::get(self.iosf, Delay::Wclk, 0, 0)
    }

    fn assert_fifo_reset(&self) {
        for strobe in (0..STROBES).step_by(2) {
            self.write(FIFO_RST, strobe, 0);
        }
    }

    fn fifo_reset(&self) {
        self.assert_fifo_reset();
        for strobe in (0..STROBES).step_by(2) {
            self.write(FIFO_RST, strobe, 1);
        }
    }

    /// Run the programmed write/read test: the strobes that failed, all of
    /// them when the engine did not complete (its error status is stale).
    fn test(&self) -> u8 {
        match self.cpgc.run() {
            Ok(()) => self.cpgc.errors(),
            Err(_) => u8::MAX,
        }
    }

    /// Run the programmed test until it completes.
    fn run_sampling_test(&self) -> Result<(), ServiceError> {
        self.cpgc.clear_errors();
        self.cpgc.start();
        let mut done = Err(ServiceError::Timeout);
        for _ in 0..1000 {
            done = self.cpgc.poll();
            if done.is_ok() {
                break;
            }
        }
        self.cpgc.stop();
        done
    }

    /// Majority vote of the sample status over three tests per strobe.
    fn sample(&self, d: Delay) -> Result<Strobes<bool>, ServiceError> {
        let mut votes = [0i32; STROBES];
        for _ in 0..NUM_SAMPLES {
            self.run_sampling_test()?;
            for (strobe, vote) in votes.iter_mut().enumerate() {
                *vote += if delay::sample(self.iosf, d, 0, strobe) != 0 {
                    1
                } else {
                    -1
                };
            }
        }
        Ok(votes.map(|vote| vote >= 0))
    }

    fn jedec(&self, command: tock_registers::fields::FieldValue<u32, JEDEC::Register>) {
        self.iosf.message(DUNIT_CH0, OP_DRAM_INIT, command.value);
    }

    fn precharge_all(&self, rank: u32) {
        self.jedec(JEDEC::COMMAND::Precharge + JEDEC::ADDRESS.val(1 << 10) + JEDEC::RANK.val(rank));
    }

    /// MCH ODT off and differential amplifiers forced on.
    fn override_odt_and_diffamp(&self) {
        for m in 0..DQ_NUM_MODULES {
            self.iosf.reg(dq_ovrctl_b0(0, m)).modify(
                DQ_OVRCTL_B0::DQS_MCHODT_OVR_B0::SET
                    + DQ_OVRCTL_B0::DM_MCHODT_OVR_B0::SET
                    + DQ_OVRCTL_B0::DQ_MCHODT_OVR_B0::SET
                    + DQ_OVRCTL_B0::DQS_MCHODT_OVR_VAL_B0::CLEAR
                    + DQ_OVRCTL_B0::DM_MCHODT_OVR_VAL_B0::CLEAR
                    + DQ_OVRCTL_B0::DQ_MCHODT_OVR_VAL_B0::CLEAR
                    + DQ_OVRCTL_B0::DQS_DIFFAMPEN_OVR_B0::SET
                    + DQ_OVRCTL_B0::DM_DIFFAMPEN_OVR_B0::SET
                    + DQ_OVRCTL_B0::DQ_DIFFAMPEN_OVR_B0::SET
                    + DQ_OVRCTL_B0::DQS_DIFFAMPEN_OVR_VAL_B0::SET
                    + DQ_OVRCTL_B0::DM_DIFFAMPEN_OVR_VAL_B0::SET
                    + DQ_OVRCTL_B0::DQ_DIFFAMPEN_OVR_VAL_B0::SET,
            );
            self.iosf.reg(dq_ovrctl_b1(0, m)).modify(
                DQ_OVRCTL_B1::DQS_MCHODT_OVR_B1::SET
                    + DQ_OVRCTL_B1::DM_MCHODT_OVR_B1::SET
                    + DQ_OVRCTL_B1::DQ_MCHODT_OVR_B1::SET
                    + DQ_OVRCTL_B1::DQS_MCHODT_OVR_VAL_B1::CLEAR
                    + DQ_OVRCTL_B1::DM_MCHODT_OVR_VAL_B1::CLEAR
                    + DQ_OVRCTL_B1::DQ_MCHODT_OVR_VAL_B1::CLEAR
                    + DQ_OVRCTL_B1::DQS_DIFFAMPEN_OVR_B1::SET
                    + DQ_OVRCTL_B1::DM_DIFFAMPEN_OVR_B1::SET
                    + DQ_OVRCTL_B1::DQ_DIFFAMPEN_OVR_B1::SET
                    + DQ_OVRCTL_B1::DQS_DIFFAMPEN_OVR_VAL_B1::SET
                    + DQ_OVRCTL_B1::DM_DIFFAMPEN_OVR_VAL_B1::SET
                    + DQ_OVRCTL_B1::DQ_DIFFAMPEN_OVR_VAL_B1::SET,
            );
        }
    }

    /// Probe one `step` to choose each strobe's search direction: up while
    /// it samples 0, down once it samples 1. A strobe still at 0 after the
    /// step keeps the step.
    fn fine_search(&self, d: Delay, step: i32) -> Result<Strobes<bool>, ServiceError> {
        let before = self.sample(d)?;
        let delays: Strobes<i32> = core::array::from_fn(|s| self.delay(d, s));
        for (s, delay) in delays.iter().enumerate() {
            self.set_delay(d, s, delay + step);
        }
        let after = self.sample(d)?;
        let mut up = [false; STROBES];
        for s in 0..STROBES {
            up[s] = !before[s];
            if before[s] || after[s] {
                self.set_delay(d, s, delays[s]);
            }
        }
        Ok(up)
    }

    /// Step each strobe in its direction until it samples its direction's
    /// target (1 going up, 0 going down) or leaves the delay range.
    fn direction_step(&self, d: Delay, up: &Strobes<bool>, step: i32) -> Result<(), ServiceError> {
        let mut done = [false; STROBES];
        for _ in 0..256 {
            if done.iter().all(|&done| done) {
                return Ok(());
            }
            for s in 0..STROBES {
                if done[s] {
                    continue;
                }
                let next = self.delay(d, s) + if up[s] { step } else { -step };
                if !(0..=MAX_LINEAR_DELAY).contains(&next) {
                    done[s] = true;
                } else {
                    self.set_delay(d, s, next);
                }
            }
            let samples = self.sample(d)?;
            for s in 0..STROBES {
                if samples[s] == up[s] {
                    done[s] = true;
                }
            }
        }
        Ok(())
    }

    /// Coarse probe, medium search, then a single-step search back over the
    /// edge.
    fn find_edge(&self, d: Delay) -> Result<(), ServiceError> {
        let mut up = self.fine_search(d, HALF_CLK / 2)?;
        self.direction_step(d, &up, HALF_CLK / 4)?;
        for dir in &mut up {
            *dir = !*dir;
        }
        self.direction_step(d, &up, 1)
    }

    fn add_to_all(&self, d: Delay, delta: i32) {
        for s in 0..STROBES {
            self.set_delay(d, s, self.delay(d, s) + delta);
        }
    }
}

/// Receive enable: find the DQS preamble of each strobe.
pub fn receive_enable<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: receive enable training");
    let t = Trainer::new(iosf);
    let rank = 0;
    for m in 0..DQ_NUM_MODULES {
        for reg in [dq_b0rxiobufctl(0, m), dq_b1rxiobufctl(0, m)] {
            iosf.reg(reg).modify(DQ_B0RXIOBUFCTL::PWRDN::CLEAR);
        }
    }
    t.override_odt_and_diffamp();
    t.cpgc.enable();
    t.cpgc.setup(Test::SingleRead);
    for s in 0..STROBES {
        t.write_all_ranks(RCVN_MIN, s, 1);
        t.set_delay(Delay::Rcvn, s, 7 * HALF_CLK);
    }
    t.precharge_all(rank);
    // FIFO reset stays asserted during the search.
    t.assert_fifo_reset();

    t.find_edge(Delay::Rcvn)?;
    // Centre on the preamble.
    t.add_to_all(Delay::Rcvn, QTR_CLK);
    let samples = t.sample(Delay::Rcvn)?;
    if let Some(s) = samples.iter().position(|&high| !high) {
        fstart_log::error!("baytrail: RCVN strobe {} did not converge", s);
        return Err(ServiceError::HardwareError);
    }
    // Back one clock, then whole clocks back until the strobe samples 0.
    t.add_to_all(Delay::Rcvn, -FULL_CLK);
    let mut done = [false; STROBES];
    for _ in 0..16 {
        if done.iter().all(|&done| done) {
            break;
        }
        let samples = t.sample(Delay::Rcvn)?;
        for s in 0..STROBES {
            if done[s] {
                continue;
            }
            if samples[s] {
                t.set_delay(Delay::Rcvn, s, t.delay(Delay::Rcvn, s) - FULL_CLK);
            } else {
                done[s] = true;
            }
        }
    }
    t.add_to_all(Delay::Rcvn, QTR_CLK);
    t.fifo_reset();
    for (s, rcvn) in p.rcvn.iter_mut().enumerate() {
        *rcvn = t.delay(Delay::Rcvn, s);
        fstart_log::debug!(
            "  RCVN strobe {}: {} (2x={} pi={})",
            s,
            *rcvn,
            *rcvn / HALF_CLK,
            *rcvn % HALF_CLK
        );
    }
    t.cpgc.disable();
    Ok(())
}

fn fine_write_leveling<P: ConfigRegionAccess>(
    t: &Trainer<'_, P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: fine write leveling");
    let iosf = t.iosf;
    let rank = 0;
    // No periodic RCOMP during training.
    iosf.reg(cmpctrl()).modify(CMPCTRL::RCOMP_EN::CLEAR);
    t.override_odt_and_diffamp();
    // ODT on for rank 0.
    iosf.reg(DRMC_REG)
        .modify(DRMC::ODT_VAL.val(1) + DRMC::ODT_MODE::SET);
    for s in 0..STROBES {
        t.write_all_ranks(WDQS_MIN, s, 1);
        t.write_all_ranks(WDQ_MIN, s, 1);
    }
    t.cpgc.enable();
    t.cpgc.setup(Test::SingleWrite);
    t.precharge_all(rank);
    let mr1 = |wl| {
        JEDEC::COMMAND::Mrs
            + JEDEC::BANK.val(1)
            + JEDEC::ADDRESS
                .val((MR1::DIC0::SET + MR1::RTT_NOM0::SET + MR1::WL_EN.val(wl)).value >> 6)
            + JEDEC::RANK.val(rank)
    };
    t.jedec(mr1(1));
    let resetctl = iosf.reg(clkctl_ddr3resetctl(0));
    resetctl.modify(CLKCTL_DDR3RESETCTL::WR_LEVEL::SET);

    let wclk = t.wclk();
    for s in 0..STROBES {
        t.set_delay(Delay::Wdqs, s, wclk);
    }
    t.find_edge(Delay::Wdqs)?;
    for (s, wdqs) in p.wdqs.iter_mut().enumerate() {
        *wdqs = t.delay(Delay::Wdqs, s);
        fstart_log::debug!("  WDQS strobe {}: {}", s, *wdqs);
        if (*wdqs - wclk).abs() > FULL_CLK {
            fstart_log::error!(
                "baytrail: write leveling strobe {} WDQS {} too far from WCLK {}",
                s,
                *wdqs,
                wclk
            );
            return Err(ServiceError::HardwareError);
        }
    }
    resetctl.modify(CLKCTL_DDR3RESETCTL::WR_LEVEL::CLEAR);
    t.jedec(mr1(0));
    iosf.reg(DRMC_REG)
        .modify(DRMC::ODT_VAL.val(0) + DRMC::ODT_MODE::CLEAR);
    t.cpgc.disable();
    iosf.reg(cmpctrl()).modify(CMPCTRL::RCOMP_EN::SET);
    Ok(())
}

fn coarse_write_leveling<P: ConfigRegionAccess>(
    t: &Trainer<'_, P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: coarse write leveling");
    t.cpgc.enable();
    t.cpgc.setup(Test::WriteRead { bursts: 4 });
    let set_wdq = |s, wdqs: i32| {
        t.set_delay(
            Delay::Wdq,
            s,
            if wdqs > QTR_CLK { wdqs - QTR_CLK } else { 0 },
        );
    };
    for s in 0..STROBES {
        set_wdq(s, t.delay(Delay::Wdqs, s));
    }
    t.precharge_all(0);
    t.fifo_reset();
    let mut converged = false;
    for _ in 0..16 {
        let errors = t.test();
        if errors == 0 {
            converged = true;
            break;
        }
        for s in (0..STROBES).filter(|s| errors & (1 << s) != 0) {
            let wdqs = t.delay(Delay::Wdqs, s) + FULL_CLK;
            t.set_delay(Delay::Wdqs, s, wdqs);
            set_wdq(s, wdqs);
        }
        t.fifo_reset();
    }
    if !converged {
        fstart_log::error!("baytrail: coarse write leveling did not converge");
        return Err(ServiceError::HardwareError);
    }
    for s in 0..STROBES {
        p.wdqs[s] = t.delay(Delay::Wdqs, s);
        p.wdq[s] = t.delay(Delay::Wdq, s);
        fstart_log::debug!("  strobe {}: WDQS={} WDQ={}", s, p.wdqs[s], p.wdq[s]);
    }
    t.cpgc.disable();
    Ok(())
}

pub fn write_leveling<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    let t = Trainer::new(iosf);
    fine_write_leveling(&t, p)?;
    coarse_write_leveling(&t, p)
}

/// The passing window of one strobe over `values`, by write/read tests.
fn window<P: ConfigRegionAccess>(
    t: &Trainer<'_, P>,
    strobe: usize,
    values: impl Iterator<Item = i32>,
    mut set: impl FnMut(i32),
) -> Option<(i32, i32)> {
    // The widest contiguous passing run: the reference spans the first to
    // the last pass, which centres a split eye on a failing setting.
    let mut best: Option<(i32, i32)> = None;
    let mut run: Option<(i32, i32)> = None;
    for value in values {
        set(value);
        t.fifo_reset();
        if t.test() & (1 << strobe) == 0 {
            run = Some((run.map_or(value, |(left, _)| left), value));
            if best.is_none_or(|(l, r)| run.is_some_and(|(nl, nr)| nr - nl > r - l)) {
                best = run;
            }
        } else {
            run = None;
        }
    }
    best
}

/// Centre the read DQS PI of each strobe in its passing window.
pub fn read_training<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: read training");
    let t = Trainer::new(iosf);
    t.cpgc.enable();
    t.cpgc.setup(Test::WriteRead { bursts: 4 });
    for (s, rdq) in p.rdq.iter_mut().enumerate() {
        let Some((left, right)) = window(&t, s, 0..64, |pi| t.write(RDQS_PI, s, pi as u32)) else {
            fstart_log::error!("baytrail: read training strobe {} has no passing window", s);
            return Err(ServiceError::HardwareError);
        };
        let center = (left + right) / 2;
        t.write(RDQS_PI, s, center as u32);
        *rdq = center;
        fstart_log::debug!("  strobe {}: RDQS {}..{} center {}", s, left, right, center);
    }
    t.fifo_reset();
    t.cpgc.disable();
    Ok(())
}

/// Centre the write DQ delay of each strobe within two clocks of its DQS.
pub fn write_training<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &mut Params,
) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: write training");
    let t = Trainer::new(iosf);
    t.cpgc.enable();
    t.cpgc.setup(Test::WriteRead { bursts: 4 });
    for (s, wdq) in p.wdq.iter_mut().enumerate() {
        let wdqs = t.delay(Delay::Wdqs, s);
        let low = if wdqs > 2 * FULL_CLK {
            wdqs - 2 * FULL_CLK
        } else {
            0
        };
        let high = wdqs + 2 * FULL_CLK;
        let Some((left, right)) =
            window(&t, s, low..=high, |value| t.set_delay(Delay::Wdq, s, value))
        else {
            fstart_log::error!(
                "baytrail: write training strobe {} has no passing window",
                s
            );
            return Err(ServiceError::HardwareError);
        };
        let center = (left + right) / 2;
        t.set_delay(Delay::Wdq, s, center);
        *wdq = center;
        fstart_log::debug!("  strobe {}: WDQ {}..{} center {}", s, left, right, center);
    }
    t.fifo_reset();
    t.cpgc.disable();
    Ok(())
}
