//! IOSAV: the memory controller's DRAM command sequencer
//! (coreboot `raminit_iosav.c`).
//!
//! A sequence is up to four sub-sequences, each one DRAM command repeated
//! with a gap, an address update rule and a wait afterwards.

use super::Ctrl;
use crate::sandybridge::regs::{
    ADDR_UPDATE, IOSAV_STATUS, Mchbar, SEQ_CTL, SP_CMD_ADDR, SP_CMD_CTRL, SUBSEQ_CTRL,
};
use tock_registers::interfaces::{Readable, Writeable};

/// IOSAV_n_SP_CMD_CTRL command encodings (!RAS/!CAS/!WE, CKE, ODT, CS).
pub const MRS: u32 = 0xf000;
pub const PRE: u32 = 0xf002;
pub const ZQCS: u32 = 0xf003;
pub const ACT: u32 = 0xf006;
pub const RD: u32 = 0xf105;
pub const NOP_ALT: u32 = 0xf107;
pub const WR: u32 = 0xf201;
pub const NOP: u32 = 0xf207;

/// Data direction of a sub-sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    None,
    Read,
    Write,
}

/// One sub-sequence (coreboot `struct iosav_ssq`).
#[derive(Debug, Clone, Copy)]
pub struct Ssq {
    pub command: u32,
    pub ranksel_ap: u32,
    pub executions: u32,
    pub gap: u32,
    pub post: u32,
    pub dir: Dir,
    pub address: u32,
    pub rowbits: u32,
    pub bank: u32,
    pub rank: u32,
    pub update: Update,
}

/// Address update rule (IOSAV_n_ADDR_UPDATE).
#[derive(Debug, Clone, Copy, Default)]
pub struct Update {
    pub inc_addr_1: bool,
    pub inc_addr_8: bool,
    pub inc_bank: bool,
    pub inc_rank: u32,
    pub wrap: u32,
    pub lfsr_upd: u32,
    pub lfsr_xors: u32,
}

impl Update {
    pub const fn wrap(wrap: u32) -> Self {
        Self {
            inc_addr_1: false,
            inc_addr_8: false,
            inc_bank: false,
            inc_rank: 0,
            wrap,
            lfsr_upd: 0,
            lfsr_xors: 0,
        }
    }
    pub const fn addr8(wrap: u32) -> Self {
        Self {
            inc_addr_8: true,
            ..Self::wrap(wrap)
        }
    }
    pub const fn bank(wrap: u32) -> Self {
        Self {
            inc_bank: true,
            ..Self::wrap(wrap)
        }
    }
}

impl Ssq {
    /// A one-shot command on `rank` with `rowbits` 6, the common shape.
    pub const fn new(command: u32, rank: u32) -> Self {
        Self {
            command,
            ranksel_ap: 1,
            executions: 1,
            gap: 3,
            post: 0,
            dir: Dir::None,
            address: 0,
            rowbits: 6,
            bank: 0,
            rank,
            update: Update::wrap(0),
        }
    }

    fn write(&self, mch: &Mchbar, ch: usize, index: usize) {
        let mc = &mch.mc[ch];
        mc.sp_cmd_ctrl[index].write(
            SP_CMD_CTRL::COMMAND.val(self.command) + SP_CMD_CTRL::RANKSEL_AP.val(self.ranksel_ap),
        );
        let dir = match self.dir {
            Dir::None => SUBSEQ_CTRL::DATA_DIRECTION::None,
            Dir::Read => SUBSEQ_CTRL::DATA_DIRECTION::Read,
            Dir::Write => SUBSEQ_CTRL::DATA_DIRECTION::Write,
        };
        mc.subseq_ctrl[index].write(
            SUBSEQ_CTRL::CMD_EXECUTIONS.val(self.executions)
                + SUBSEQ_CTRL::CMD_DELAY_GAP.val(self.gap)
                + SUBSEQ_CTRL::POST_SSQ_WAIT.val(self.post)
                + dir,
        );
        mc.sp_cmd_addr[index].write(
            SP_CMD_ADDR::ADDRESS.val(self.address)
                + SP_CMD_ADDR::ROWBITS.val(self.rowbits)
                + SP_CMD_ADDR::BANK.val(self.bank)
                + SP_CMD_ADDR::RANK.val(self.rank),
        );
        let u = self.update;
        mc.addr_update[index].write(
            ADDR_UPDATE::INC_ADDR_1.val(u32::from(u.inc_addr_1))
                + ADDR_UPDATE::INC_ADDR_8.val(u32::from(u.inc_addr_8))
                + ADDR_UPDATE::INC_BANK.val(u32::from(u.inc_bank))
                + ADDR_UPDATE::INC_RANK.val(u.inc_rank)
                + ADDR_UPDATE::ADDR_WRAP.val(u.wrap)
                + ADDR_UPDATE::LFSR_UPD.val(u.lfsr_upd)
                + ADDR_UPDATE::LFSR_XORS.val(u.lfsr_xors),
        );
    }
}

/// The sequencer of one channel (or the broadcast bank), with the number of
/// programmed sub-sequences coreboot keeps in `ssq_count`.
pub struct Iosav<'a> {
    mch: &'a Mchbar,
    count: usize,
}

impl<'a> Iosav<'a> {
    pub const fn new(mch: &'a Mchbar) -> Self {
        Self { mch, count: 0 }
    }

    pub fn write(&mut self, ch: usize, seq: &[Ssq]) {
        for (index, ssq) in seq.iter().enumerate() {
            ssq.write(self.mch, ch, index);
        }
        self.count = seq.len();
    }

    pub fn run(&self, ch: usize, loops: u32, as_timer: bool) {
        if self.count == 0 {
            return;
        }
        self.mch.mc[ch].seq_ctl.write(
            SEQ_CTL::LOOPS.val(loops)
                + SEQ_CTL::LAST_SUBSEQ.val(self.count as u32 - 1)
                + SEQ_CTL::AS_TIMER.val(u32::from(as_timer)),
        );
    }

    /// Wait for the sequence to finish or a panic refresh to abort it.
    pub fn wait(&self, ch: usize) {
        loop {
            let status = self.mch.mc[ch].status.extract();
            if status.is_set(IOSAV_STATUS::PANIC) || status.is_set(IOSAV_STATUS::DRAINED) {
                return;
            }
            core::hint::spin_loop();
        }
    }

    pub fn run_once_and_wait(&self, ch: usize) {
        self.run(ch, 1, false);
        self.wait(ch);
    }

    pub fn zqcs(&mut self, ch: usize, rank: u32, gap: u32, post: u32, wrap: u32) {
        let mut ssq = Ssq::new(ZQCS, rank);
        ssq.ranksel_ap = 0;
        ssq.gap = gap;
        ssq.post = post;
        ssq.update = Update::wrap(wrap);
        self.write(ch, &[ssq]);
    }

    pub fn prea(&mut self, ch: usize, rank: u32, post: u32, wrap: u32) {
        let mut ssq = Ssq::new(PRE, rank);
        ssq.post = post;
        ssq.address = 1 << 10;
        ssq.update = Update::wrap(wrap);
        self.write(ch, &[ssq]);
    }

    /// MR3 MPR enable, `loops` reads `gap` apart, `loops2` more, MPR disable.
    #[allow(clippy::too_many_arguments)]
    pub fn read_mpr(
        &mut self,
        ch: usize,
        rank: u32,
        t_mod: u32,
        loops: u32,
        gap: u32,
        loops2: u32,
        post2: u32,
    ) {
        let mrs = |address| Ssq {
            post: t_mod,
            address,
            bank: 3,
            ..Ssq::new(MRS, rank)
        };
        self.write(
            ch,
            &[
                mrs(4),
                Ssq {
                    executions: loops,
                    gap,
                    post: 4,
                    dir: Dir::Read,
                    rowbits: 0,
                    ..Ssq::new(RD, rank)
                },
                Ssq {
                    executions: loops2,
                    gap: 4,
                    post: post2,
                    ..Ssq::new(RD, rank)
                },
                mrs(0),
            ],
        );
    }

    fn act_gap(c: &Ctrl) -> u32 {
        c.t_rrd.max((c.t_faw >> 2) + 1)
    }

    pub fn prea_act_read(&mut self, c: &Ctrl, ch: usize, rank: u32) {
        let prea = Ssq {
            post: c.t_rp,
            address: 1 << 10,
            update: Update::wrap(18),
            ..Ssq::new(PRE, rank)
        };
        self.write(
            ch,
            &[
                prea,
                Ssq {
                    executions: 8,
                    gap: Self::act_gap(c),
                    post: c.cas,
                    update: Update::bank(18),
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    executions: 500,
                    gap: 4,
                    post: c.t_rtp.max(8),
                    dir: Dir::Read,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(RD, rank)
                },
                prea,
            ],
        );
    }

    pub fn jedec_write_leveling(&mut self, c: &Ctrl, ch: usize, rank: u32, bank: u32, mr1: u32) {
        // tWLMRD: first DQS rising edge after write leveling mode is set.
        const T_WLMRD: u32 = 40;
        self.write(
            ch,
            &[
                Ssq {
                    post: T_WLMRD,
                    address: mr1,
                    bank,
                    ..Ssq::new(MRS, rank)
                },
                Ssq {
                    post: c.cwl + c.t_wlo,
                    dir: Dir::Write,
                    address: 8,
                    rowbits: 0,
                    ..Ssq::new(NOP, rank)
                },
                Ssq {
                    post: c.cas + 38,
                    dir: Dir::Read,
                    address: 4,
                    rowbits: 0,
                    ..Ssq::new(NOP_ALT, rank)
                },
                Ssq {
                    post: c.t_mod,
                    address: mr1 | 1 << 12,
                    bank,
                    ..Ssq::new(MRS, rank)
                },
            ],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn misc_write(
        &mut self,
        c: &Ctrl,
        ch: usize,
        rank: u32,
        gap0: u32,
        loops0: u32,
        gap1: u32,
        loops2: u32,
        wrap2: u32,
    ) {
        let single = loops0 == 1;
        self.write(
            ch,
            &[
                Ssq {
                    executions: loops0,
                    gap: gap0,
                    post: c.t_rcd,
                    update: Update {
                        inc_bank: !single,
                        ..Update::wrap(if single { 0 } else { 18 })
                    },
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    gap: gap1,
                    post: 4,
                    dir: Dir::Write,
                    address: 8,
                    rowbits: 0,
                    update: Update::wrap(31),
                    ..Ssq::new(NOP, rank)
                },
                Ssq {
                    executions: loops2,
                    gap: 4,
                    post: 4,
                    dir: Dir::Write,
                    rowbits: 0,
                    update: Update::addr8(wrap2),
                    ..Ssq::new(WR, rank)
                },
                Ssq {
                    post: c.cwl + c.t_wtr + 5,
                    dir: Dir::Write,
                    address: 8,
                    rowbits: 0,
                    update: Update::wrap(31),
                    ..Ssq::new(NOP, rank)
                },
            ],
        );
    }

    pub fn command_training(&mut self, c: &Ctrl, ch: usize, rank: u32, address: u32) {
        let lfsr = Update {
            lfsr_upd: 3,
            lfsr_xors: 2,
            ..Update::addr8(18)
        };
        self.write(
            ch,
            &[
                Ssq {
                    executions: 8,
                    gap: Self::act_gap(c),
                    post: c.t_rcd,
                    address,
                    update: Update::bank(18),
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    executions: 32,
                    gap: 4,
                    post: c.cwl + c.t_wtr + 8,
                    dir: Dir::Write,
                    rowbits: 0,
                    update: lfsr,
                    ..Ssq::new(WR, rank)
                },
                Ssq {
                    executions: 32,
                    gap: 4,
                    post: c.t_rtp.max(8),
                    dir: Dir::Read,
                    rowbits: 0,
                    update: lfsr,
                    ..Ssq::new(RD, rank)
                },
                Ssq {
                    gap: 4,
                    post: 15,
                    address: 1 << 10,
                    update: Update::wrap(18),
                    ..Ssq::new(PRE, rank)
                },
            ],
        );
    }

    pub fn data_write(&mut self, c: &Ctrl, ch: usize, rank: u32) {
        self.write(
            ch,
            &[
                Ssq {
                    executions: 4,
                    gap: Self::act_gap(c),
                    post: c.t_rcd,
                    update: Update::wrap(18),
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    executions: 32,
                    gap: 20,
                    post: c.cwl + c.t_wtr + 8,
                    dir: Dir::Write,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(WR, rank)
                },
                Ssq {
                    executions: 32,
                    gap: 20,
                    post: c.t_rtp.max(8),
                    dir: Dir::Read,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(RD, rank)
                },
                Ssq {
                    post: c.t_rp,
                    address: 1 << 10,
                    ..Ssq::new(PRE, rank)
                },
            ],
        );
    }

    pub fn aggressive_write_read(&mut self, c: &Ctrl, ch: usize, rank: u32) {
        self.write(
            ch,
            &[
                Ssq {
                    executions: 4,
                    gap: Self::act_gap(c),
                    post: c.t_rcd,
                    update: Update::bank(18),
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    executions: 480,
                    gap: 4,
                    post: c.t_wtr + c.cwl + 8,
                    dir: Dir::Write,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(WR, rank)
                },
                Ssq {
                    executions: 480,
                    gap: 4,
                    post: c.t_rtp.max(8),
                    dir: Dir::Read,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(RD, rank)
                },
                Ssq {
                    gap: 4,
                    post: c.t_rp,
                    address: 1 << 10,
                    ..Ssq::new(PRE, rank)
                },
            ],
        );
    }

    pub fn memory_test(&mut self, ch: usize, rank: u32) {
        self.write(
            ch,
            &[
                Ssq {
                    executions: 4,
                    gap: 8,
                    post: 40,
                    update: Update::bank(18),
                    ..Ssq::new(ACT, rank)
                },
                Ssq {
                    executions: 100,
                    gap: 4,
                    post: 40,
                    dir: Dir::Write,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(WR, rank)
                },
                Ssq {
                    executions: 100,
                    gap: 4,
                    post: 40,
                    dir: Dir::Read,
                    rowbits: 0,
                    update: Update::addr8(18),
                    ..Ssq::new(RD, rank)
                },
                Ssq {
                    post: 40,
                    address: 1 << 10,
                    update: Update::wrap(18),
                    ..Ssq::new(PRE, rank)
                },
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::LocalRegisterCopy;

    #[test]
    fn ssq_fields_match_coreboot_bit_positions() {
        let value = (SUBSEQ_CTRL::CMD_EXECUTIONS.val(500)
            + SUBSEQ_CTRL::CMD_DELAY_GAP.val(4)
            + SUBSEQ_CTRL::POST_SSQ_WAIT.val(400)
            + SUBSEQ_CTRL::DATA_DIRECTION::Write)
            .value;
        assert_eq!(value, 500 | 4 << 10 | 400 << 16 | 2 << 26);
        let addr = (SP_CMD_ADDR::ADDRESS.val(1 << 10)
            + SP_CMD_ADDR::ROWBITS.val(6)
            + SP_CMD_ADDR::BANK.val(3)
            + SP_CMD_ADDR::RANK.val(2))
        .value;
        assert_eq!(addr, 1 << 10 | 6 << 16 | 3 << 20 | 2 << 24);
        let update = LocalRegisterCopy::<u32, ADDR_UPDATE::Register>::new(
            (ADDR_UPDATE::INC_ADDR_8::SET
                + ADDR_UPDATE::ADDR_WRAP.val(18)
                + ADDR_UPDATE::LFSR_UPD.val(3)
                + ADDR_UPDATE::LFSR_XORS.val(2))
            .value,
        );
        assert_eq!(update.get(), 2 | 18 << 5 | 3 << 10 | 2 << 16);
    }
}
