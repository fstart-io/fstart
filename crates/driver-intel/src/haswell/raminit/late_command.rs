//! Late command training: a 2D CTL/CLK sweep, then clock, command and
//! grouped control centering under CA Vref stress. DDR3 CKE2/3 share a PI.
use super::{
    change_margin::{set_rx, set_tx, update_vref},
    configure_mc::{TrainingState, io},
    ddr3::{self, ModeRegisters, ResetPin},
    ranges::{Phase, Range},
    spd::Topology,
    testing_io::{Profile, Stepping, select_ranks},
    *,
};
/// CLK sweep step and last sampled PI of the 0..=127 clock range.
const CLK_STEP: i32 = 6;
const LAST_CLK_SAMPLE: i32 = 127 / CLK_STEP * CLK_STEP;
use DDR_DATA_RX_TRAIN_RANK as rx;
use DDR_DATA_TX_TRAIN_RANK as tx;
#[derive(Clone, Copy)]
enum Signal {
    Clock,
    North,
    South,
    Ctl,
}
#[derive(Clone, Copy)]
struct Channel {
    clk: [i32; 4],
    ctl: [i32; 4],
    north: i32,
    south: i32,
}
fn encode(values: &[i32]) -> u32 {
    values
        .iter()
        .enumerate()
        .fold(0, |w, (i, v)| w | ((*v as u32) & 127) << (7 * i))
}
struct Search<'a, 'p> {
    m: MchBar,
    t: &'a Topology,
    s: &'a mut TrainingState,
    mrs: &'a ModeRegisters,
    pin: &'a mut ResetPin<'p>,
    ca: [Channel; 2],
    cmd: u32,
}
impl Search<'_, '_> {
    fn shift(&mut self, ch: usize, what: Signal, mask: u8, value: i32, commit: bool) {
        let m = self.m;
        let ca = &mut self.ca[ch];
        let rmask = self.t.rankmap[ch] & mask;
        match what {
            Signal::Clock => {
                let mut pi = ca.clk;
                for r in (0..4).filter(|r| rmask & (1 << r) != 0) {
                    pi[r] = (pi[r] + value).rem_euclid(128);
                    // The coupled DQ offset is a signed byte in the MRC. A
                    // wrapped CLK eye may return a center above 127.
                    let offset = i32::from(value as i8);
                    for b in 0..usize::from(self.t.lanes) {
                        let rx = &mut self.s.rx[ch][r][b];
                        let val = rx::RCVEN
                            .val((rx::RCVEN.read(*rx) as i32 + offset).clamp(0, 511) as u32)
                            .modify(*rx);
                        set_rx(m, ch, r, b, val);
                        if commit {
                            *rx = val;
                        }
                        let tx = &mut self.s.tx[ch][r][b];
                        let val = (tx::DQ_DELAY
                            .val(tx::DQ_DELAY.read(*tx).wrapping_add_signed(offset))
                            + tx::DQS_DELAY
                                .val(tx::DQS_DELAY.read(*tx).wrapping_add_signed(offset)))
                        .modify(*tx);
                        set_tx(m, ch, r, b, val);
                        if commit {
                            *tx = val;
                        }
                    }
                }
                m.write32(io(0x180c, ch, 0, 0), encode(&pi));
                if commit {
                    ca.clk = pi;
                }
            }
            Signal::North | Signal::South => {
                let pi = value.clamp(0, 127);
                let word = encode(&[pi; 2]);
                if matches!(what, Signal::North) {
                    m.write32(io(0x1408, ch, 0, 0), word);
                    if commit {
                        ca.north = pi;
                    }
                } else {
                    m.write32(io(0x1a08, ch, 0, 0), word);
                    m.write32(io(0x1208, ch, 0, 0), word);
                    if commit {
                        ca.south = pi;
                    }
                }
            }
            Signal::Ctl => {
                let mut pi = ca.ctl;
                for r in (0..4).filter(|r| rmask & (1 << r) != 0) {
                    pi[r] = value.clamp(0, 127);
                }
                m.write32(io(0x1c18, ch, 0, 0), encode(&pi));
                if commit {
                    ca.ctl = pi;
                }
                pi[2] = (pi[2] + pi[3]) / 2;
                pi[3] = 0;
                m.write32(io(0x1218, ch, 0, 0), encode(&pi));
            }
        }
    }
    fn select(&self, ranks: u8) -> u8 {
        select_ranks(self.m, self.t, 0, ranks) | select_ranks(self.m, self.t, 1, ranks)
    }
    fn reset(&mut self) -> Result<u8, Error> {
        ddr3::sync_io(self.m)?;
        ddr3::reset_and_program(self.m, self.t, self.mrs, true, self.pin)
    }
    fn test(&mut self, mask: u8, stress: bool) -> Result<u8, Error> {
        // A JEDEC command may fail at a deliberately bad PI. It is a failed
        // sample, not a passing eye; propagate I/O/reset timeouts instead.
        let failed = self.reset()? & mask;
        let active = mask & !failed;
        if active == 0 {
            return Ok(failed);
        }
        // Global START runs every enabled sequencer, regardless of which
        // CH_TEST_DONE bits the poll waits for. Gate failed channels too.
        for ch in 0..2 {
            if failed & (1 << ch) != 0 {
                self.m.clrbits32(
                    0x48a8 + 8 * ch,
                    REUT_SEQ_CFG::GLOBAL_CONTROL::SET.value as u32,
                );
            }
        }
        let result = self.probe(active, stress);
        for ch in 0..2 {
            if failed & (1 << ch) != 0 {
                self.m.setbits32(
                    0x48a8 + 8 * ch,
                    REUT_SEQ_CFG::GLOBAL_CONTROL::SET.value as u32,
                );
            }
        }
        result.map(|bad| failed | (bad & active))
    }
    fn probe(&mut self, mask: u8, stress: bool) -> Result<u8, Error> {
        if !stress {
            return Profile::Cadb.run(self.m, mask);
        }
        let mut bad = 0;
        for v in [-8, 8] {
            update_vref(self.m, self.s, 2, v, false, true)?;
            bad |= Profile::Cadb.run(self.m, mask)?;
            if bad & mask == mask {
                break;
            }
        }
        Ok(bad)
    }
    fn linear(&mut self, mask: u8, ranks: u8, commit: bool) -> Result<[i32; 2], Error> {
        let mut p = [Phase::default(); 2];
        for delay in (0..=LAST_CLK_SAMPLE).step_by(CLK_STEP as usize) {
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    self.shift(ch, Signal::Clock, ranks, delay, false);
                }
            }
            let bad = self.test(mask, commit)?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    p[ch].record(bad & (1 << ch) == 0, delay, 0, CLK_STEP);
                }
            }
        }
        let mut width = [0; 2];
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            p[ch].wrap(0, 128, false);
            let mut range = p[ch].largest();
            if range.width() >= 128 {
                // No failing sample: coreboot's cmd_find_edges_linear sees
                // the sampled span [0, 126] as a good eye (center 63), and
                // as the widest one in the 2D CTL/CLK search.
                range = Range {
                    start: 0,
                    end: LAST_CLK_SAMPLE,
                };
            }
            let good = (18..127).contains(&range.width());
            if good {
                width[ch] = range.width();
            }
            // Restore both the clock and its coupled DQ PIs between 2D slices.
            self.shift(
                ch,
                Signal::Clock,
                ranks,
                if commit {
                    if good { range.center() } else { 63 }
                } else {
                    0
                },
                commit,
            );
        }
        if commit {
            update_vref(self.m, self.s, 2, 0, false, true)?;
        }
        let failed = self.reset()?;
        if commit
            && (0..2).any(|ch| mask & (1 << ch) != 0 && (width[ch] == 0 || failed & (1 << ch) != 0))
        {
            return Err(Error::LateCommand);
        }
        Ok(width)
    }
    fn edge(
        &mut self,
        what: Signal,
        mask: u8,
        ranks: u8,
        mut lo: [i32; 2],
        mut hi: [i32; 2],
        up: bool,
    ) -> Result<[i32; 2], Error> {
        loop {
            let mut mid = [0; 2];
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    mid[ch] = (hi[ch] + lo[ch] + i32::from(up)) / 2;
                    self.shift(ch, what, ranks, mid[ch], false);
                }
            }
            let bad = self.test(mask, true)?;
            let mut done = true;
            for ch in 0..2 {
                if mask & (1 << ch) == 0 || hi[ch] <= lo[ch] {
                    continue;
                }
                if (bad & (1 << ch) != 0) == up {
                    hi[ch] = mid[ch] - i32::from(up);
                } else {
                    lo[ch] = mid[ch] + i32::from(!up);
                }
                done &= hi[ch] <= lo[ch];
            }
            if done {
                break;
            }
        }
        update_vref(self.m, self.s, 2, 0, false, true)?;
        // The last tested setting can be outside the eye. The next test
        // reissues JEDEC init after moving the PI to the next probe point.
        self.reset()?;
        Ok(if up { lo } else { hi })
    }
    fn binary(&mut self, what: Signal, ranks: u8, mid: [i32; 2]) -> Result<(), Error> {
        let mask = self.select(ranks);
        if mask == 0 {
            return Ok(());
        }
        let l = self.edge(what, mask, ranks, [0; 2], mid, false)?;
        let h = self.edge(what, mask, ranks, mid, [127; 2], true)?;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            if h[ch] - l[ch] < 18 {
                return Err(Error::LateCommand);
            }
            let center = if l[ch] == 0 && h[ch] == 127 {
                mid[ch]
            } else {
                (h[ch] + l[ch] + 1) / 2
            };
            self.shift(ch, what, ranks, center, true);
        }
        Ok(())
    }
    fn set_channel(&mut self, ch: usize, value: i32) {
        let cmd = value + if self.cmd > 1 { 21 } else { 0 };
        self.shift(ch, Signal::South, 15, cmd, true);
        self.shift(ch, Signal::North, 15, cmd, true);
        self.shift(ch, Signal::Ctl, 15, value, true);
    }
    fn search(&mut self) -> Result<(), Error> {
        let ranks = self.t.rankmap[0] | self.t.rankmap[1];
        for ch in 0..2 {
            for r in 0..4 {
                if self.t.rankmap[ch] & (1 << r) != 0 {
                    self.shift(ch, Signal::Clock, 1 << r, -64, true);
                }
            }
        }
        let mask = self.select(ranks);
        let mut best = [(0, 0, 0); 2];
        for ctl in (32..=96).step_by(8) {
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    self.set_channel(ch, ctl);
                }
            }
            let width = self.linear(mask, ranks, false)?;
            for ch in 0..2 {
                let (w, l, h) = &mut best[ch];
                if width[ch] > *w {
                    *w = width[ch];
                    *l = ctl;
                    *h = ctl;
                } else if width[ch] == *w {
                    *h = ctl;
                }
            }
        }
        if self.t.channels().any(|ch| best[ch].0 == 0) {
            return Err(Error::LateCommand);
        }
        let mid = best.map(|(_, l, h)| (l + h) / 2);
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                self.set_channel(ch, mid[ch]);
            }
        }
        let mask = self.select(ranks);
        self.linear(mask, ranks, true)?;
        self.binary(Signal::South, ranks, mid)?;
        self.binary(Signal::North, ranks, mid)?;
        for subset in [1, 2, 12] {
            if ranks & subset != 0 {
                self.binary(Signal::Ctl, ranks & subset, mid)?;
            }
        }
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            let ca = self.ca[ch];
            let mut min = ca.north.min(ca.south);
            for r in self.t.populated_ranks(ch) {
                min = min.min(ca.ctl[r]);
            }
            for r in 0..4 {
                if self.t.rankmap[ch] & (1 << r) != 0 {
                    self.shift(ch, Signal::Clock, 1 << r, -min, true);
                    self.shift(ch, Signal::Ctl, 1 << r, ca.ctl[r] - min, true);
                }
            }
            self.shift(ch, Signal::South, 15, ca.south - min, true);
            self.shift(ch, Signal::North, 15, ca.north - min, true);
        }
        Ok(())
    }
}
pub fn train(
    m: MchBar,
    t: &Topology,
    step: Stepping,
    cmd: u32,
    s: &mut TrainingState,
    mrs: &ModeRegisters,
    pin: &mut ResetPin<'_>,
) -> Result<(), Error> {
    Profile::Cadb.setup(m, t, step)?;
    let mut search = Search {
        m,
        t,
        s,
        mrs,
        pin,
        ca: [Channel {
            clk: [64; 4],
            ctl: [64; 4],
            north: 64,
            south: 64,
        }; 2],
        cmd,
    };
    let result = search.search();
    for ch in 0..2 {
        m.write8(mc(0x4198, ch), 0);
    }
    result?;
    ddr3::jedec_init(search.m, search.t, search.mrs, true, search.pin)
}
