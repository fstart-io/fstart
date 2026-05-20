//! IOSAV sequence-machine helpers used by Sandy Bridge native raminit.

use fstart_services::ServiceError;

use super::mchbar;

const IOSAV_N_SP_CMD_ADDR_BASE: usize = 0x4200;
const IOSAV_N_ADDR_UPDATE_BASE: usize = 0x4210;
const IOSAV_N_SP_CMD_CTRL_BASE: usize = 0x4220;
const IOSAV_N_SUBSEQ_CTRL_BASE: usize = 0x4230;
const IOSAV_SEQ_CTL_BASE: usize = 0x4284;
const IOSAV_STATUS_BASE: usize = 0x428c;

pub const BROADCAST_CH: usize = 3;
pub const IOSAV_MRS: u32 = 0xf000;
pub const IOSAV_PRE: u32 = 0xf002;
pub const IOSAV_ZQCS: u32 = 0xf003;
pub const IOSAV_ACT: u32 = 0xf006;
pub const IOSAV_RD: u32 = 0xf105;
pub const IOSAV_NOP_ALT: u32 = 0xf107;
pub const IOSAV_WR: u32 = 0xf201;
pub const IOSAV_NOP: u32 = 0xf207;

pub const SSQ_NA: u8 = 0;
pub const SSQ_RD: u8 = 1;
pub const SSQ_WR: u8 = 2;
pub const SSQ_RW: u8 = 3;

const IOSAV_DONE_OR_PANIC: u32 = 0x50;
const IOSAV_WAIT_LIMIT: usize = 1_000_000;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

#[inline]
const fn cxly(base: usize, channel: usize, index: usize) -> usize {
    base + (channel << 10) + (index << 2)
}

/// One IOSAV sub-sequence descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SequenceStep {
    pub command: u32,
    pub ranksel_ap: u8,
    pub cmd_executions: u16,
    pub cmd_delay_gap: u8,
    pub post_ssq_wait: u16,
    pub data_direction: u8,
    pub address: u16,
    pub rowbits: u8,
    pub bank: u8,
    pub rank: u8,
    pub inc_addr_1: bool,
    pub inc_addr_8: bool,
    pub inc_bank: bool,
    pub inc_rank: u8,
    pub addr_wrap: u8,
    pub lfsr_upd: u8,
    pub upd_rate: u8,
    pub lfsr_xors: u8,
}

impl SequenceStep {
    pub const fn simple(
        command: u32,
        rank: u8,
        bank: u8,
        address: u16,
        post_ssq_wait: u16,
    ) -> Self {
        Self {
            command,
            ranksel_ap: 0,
            cmd_executions: 1,
            cmd_delay_gap: 4,
            post_ssq_wait,
            data_direction: 0,
            address,
            rowbits: 6,
            bank,
            rank,
            inc_addr_1: false,
            inc_addr_8: false,
            inc_bank: false,
            inc_rank: 0,
            addr_wrap: 0,
            lfsr_upd: 0,
            upd_rate: 0,
            lfsr_xors: 0,
        }
    }

    const fn sp_cmd_ctrl(self) -> u32 {
        (self.command & 0xffff) | (((self.ranksel_ap as u32) & 0x3) << 16)
    }

    const fn subseq_ctrl(self) -> u32 {
        ((self.cmd_executions as u32) & 0x01ff)
            | (((self.cmd_delay_gap as u32) & 0x1f) << 10)
            | (((self.post_ssq_wait as u32) & 0x01ff) << 16)
            | (((self.data_direction as u32) & 0x03) << 26)
    }

    const fn sp_cmd_addr(self) -> u32 {
        (self.address as u32)
            | (((self.rowbits as u32) & 0x07) << 16)
            | (((self.bank as u32) & 0x07) << 20)
            | (((self.rank as u32) & 0x03) << 24)
    }

    const fn addr_update(self) -> u32 {
        (self.inc_addr_1 as u32)
            | ((self.inc_addr_8 as u32) << 1)
            | ((self.inc_bank as u32) << 2)
            | (((self.inc_rank as u32) & 0x03) << 3)
            | (((self.addr_wrap as u32) & 0x1f) << 5)
            | (((self.lfsr_upd as u32) & 0x03) << 10)
            | (((self.upd_rate as u32) & 0x0f) << 12)
            | (((self.lfsr_xors as u32) & 0x03) << 16)
    }
}

pub fn write_sequence(mchbar_base: usize, channel: usize, sequence: &[SequenceStep]) {
    for (index, step) in sequence.iter().enumerate() {
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_SP_CMD_CTRL_BASE, channel, index),
            step.sp_cmd_ctrl(),
        );
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_SUBSEQ_CTRL_BASE, channel, index),
            step.subseq_ctrl(),
        );
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_SP_CMD_ADDR_BASE, channel, index),
            step.sp_cmd_addr(),
        );
        mchbar::write32(
            mchbar_base + cxly(IOSAV_N_ADDR_UPDATE_BASE, channel, index),
            step.addr_update(),
        );
    }
}

pub fn run_queue(
    mchbar_base: usize,
    channel: usize,
    steps: usize,
    loops: u8,
    as_timer: bool,
) -> Result<(), ServiceError> {
    if steps == 0 {
        return Err(ServiceError::HardwareError);
    }
    let timer_bit = if as_timer { 1 << 22 } else { 0 };
    mchbar::write32(
        mchbar_base + cx(IOSAV_SEQ_CTL_BASE, channel),
        u32::from(loops) | (((steps as u32) - 1) << 18) | timer_bit,
    );
    Ok(())
}

pub fn read_status(mchbar_base: usize, channel: usize) -> u32 {
    mchbar::read32(mchbar_base + cx(IOSAV_STATUS_BASE, channel))
}

pub fn wait(mchbar_base: usize, channel: usize) -> Result<(), ServiceError> {
    wait_mask(mchbar_base, channel, IOSAV_DONE_OR_PANIC)
}

pub fn wait_mask(mchbar_base: usize, channel: usize, mask: u32) -> Result<(), ServiceError> {
    for _ in 0..IOSAV_WAIT_LIMIT {
        if (read_status(mchbar_base, channel) & mask) != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ServiceError::Timeout)
}

pub fn run_once_and_wait(
    mchbar_base: usize,
    channel: usize,
    steps: usize,
) -> Result<(), ServiceError> {
    run_queue(mchbar_base, channel, steps, 1, false)?;
    wait(mchbar_base, channel)
}

pub fn write_zqcs_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    gap: u8,
    post: u16,
    wrap: u8,
) {
    let mut step = SequenceStep::simple(IOSAV_ZQCS, slotrank, 0, 0, post);
    step.cmd_delay_gap = gap;
    step.addr_wrap = wrap;
    write_sequence(mchbar_base, channel, &[step]);
}

pub fn write_prea_sequence(mchbar_base: usize, channel: usize, slotrank: u8, post: u16, wrap: u8) {
    let mut step = SequenceStep::simple(IOSAV_PRE, slotrank, 0, 1 << 10, post);
    step.ranksel_ap = 1;
    step.cmd_delay_gap = 3;
    step.addr_wrap = wrap;
    write_sequence(mchbar_base, channel, &[step]);
}

pub fn write_read_mpr_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    tmod: u16,
    loops: u16,
    gap: u8,
    loops2: u16,
    post2: u16,
) {
    let mut enable = SequenceStep::simple(IOSAV_MRS, slotrank, 3, 4, tmod);
    enable.ranksel_ap = 1;
    enable.cmd_delay_gap = 3;

    let mut read0 = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, 4);
    read0.ranksel_ap = 1;
    read0.cmd_executions = loops;
    read0.cmd_delay_gap = gap;
    read0.data_direction = SSQ_RD;
    read0.rowbits = 0;

    let mut read1 = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, post2);
    read1.ranksel_ap = 1;
    read1.cmd_executions = loops2;
    read1.cmd_delay_gap = 4;

    let mut disable = SequenceStep::simple(IOSAV_MRS, slotrank, 3, 0, tmod);
    disable.ranksel_ap = 1;
    disable.cmd_delay_gap = 3;

    write_sequence(mchbar_base, channel, &[enable, read0, read1, disable]);
}

pub struct PreaActReadTiming {
    pub trp: u16,
    pub trrd: u8,
    pub tfaw: u8,
    pub cas: u16,
    pub trtp: u16,
}

pub fn write_prea_act_read_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    timing: PreaActReadTiming,
) {
    let mut prea0 = SequenceStep::simple(IOSAV_PRE, slotrank, 0, 1 << 10, timing.trp);
    prea0.ranksel_ap = 1;
    prea0.cmd_delay_gap = 3;
    prea0.addr_wrap = 18;

    let mut act = SequenceStep::simple(IOSAV_ACT, slotrank, 0, 0, timing.cas);
    act.ranksel_ap = 1;
    act.cmd_executions = 8;
    act.cmd_delay_gap = timing.trrd.max((timing.tfaw >> 2) + 1);
    act.inc_bank = true;
    act.addr_wrap = 18;

    let mut read = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, timing.trtp.max(8));
    read.ranksel_ap = 1;
    read.cmd_executions = 500;
    read.cmd_delay_gap = 4;
    read.data_direction = SSQ_RD;
    read.rowbits = 0;
    read.inc_addr_8 = true;
    read.addr_wrap = 18;

    write_sequence(mchbar_base, channel, &[prea0, act, read, prea0]);
}

pub fn write_jedec_write_leveling_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    bank: u8,
    mr1: u16,
    cwl: u16,
    twlo: u16,
    cas: u16,
    tmod: u16,
) {
    let mut enable = SequenceStep::simple(IOSAV_MRS, slotrank, bank, mr1, 40);
    enable.ranksel_ap = 1;
    enable.cmd_delay_gap = 3;

    let mut wr_nop = SequenceStep::simple(IOSAV_NOP, slotrank, 0, 8, cwl + twlo);
    wr_nop.ranksel_ap = 1;
    wr_nop.cmd_delay_gap = 3;
    wr_nop.data_direction = SSQ_WR;
    wr_nop.rowbits = 0;

    let mut rd_nop = SequenceStep::simple(IOSAV_NOP_ALT, slotrank, 0, 4, cas + 38);
    rd_nop.ranksel_ap = 1;
    rd_nop.cmd_delay_gap = 3;
    rd_nop.data_direction = SSQ_RD;
    rd_nop.rowbits = 0;

    let mut disable = SequenceStep::simple(IOSAV_MRS, slotrank, bank, mr1 | (1 << 12), tmod);
    disable.ranksel_ap = 1;
    disable.cmd_delay_gap = 3;

    write_sequence(mchbar_base, channel, &[enable, wr_nop, rd_nop, disable]);
}

pub struct MiscWriteTiming {
    pub trcd: u16,
    pub cwl: u16,
    pub twtr: u16,
}

pub struct CommandTrainingTiming {
    pub trcd: u16,
    pub trrd: u8,
    pub tfaw: u8,
    pub cwl: u16,
    pub twtr: u16,
    pub trtp: u16,
    pub trp: u16,
}

pub fn write_misc_write_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    timing: MiscWriteTiming,
    gap0: u8,
    loops0: u16,
    gap1: u8,
    loops2: u16,
    wrap2: u8,
) {
    let mut act = SequenceStep::simple(IOSAV_ACT, slotrank, 0, 0, timing.trcd);
    act.ranksel_ap = 1;
    act.cmd_executions = loops0;
    act.cmd_delay_gap = gap0;
    act.inc_bank = loops0 != 1;
    act.addr_wrap = if loops0 == 1 { 0 } else { 18 };

    let mut nop0 = SequenceStep::simple(IOSAV_NOP, slotrank, 0, 8, 4);
    nop0.ranksel_ap = 1;
    nop0.cmd_delay_gap = gap1;
    nop0.data_direction = SSQ_WR;
    nop0.rowbits = 0;
    nop0.addr_wrap = 31;

    let mut write = SequenceStep::simple(IOSAV_WR, slotrank, 0, 0, 4);
    write.ranksel_ap = 1;
    write.cmd_executions = loops2;
    write.cmd_delay_gap = 4;
    write.data_direction = SSQ_WR;
    write.rowbits = 0;
    write.inc_addr_8 = true;
    write.addr_wrap = wrap2;

    let mut nop1 = SequenceStep::simple(IOSAV_NOP, slotrank, 0, 8, timing.cwl + timing.twtr + 5);
    nop1.ranksel_ap = 1;
    nop1.cmd_delay_gap = 3;
    nop1.data_direction = SSQ_WR;
    nop1.rowbits = 0;
    nop1.addr_wrap = 31;

    write_sequence(mchbar_base, channel, &[act, nop0, write, nop1]);
}

pub fn write_data_write_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    timing: CommandTrainingTiming,
) {
    let mut act = SequenceStep::simple(IOSAV_ACT, slotrank, 0, 0, timing.trcd);
    act.ranksel_ap = 1;
    act.cmd_executions = 4;
    act.cmd_delay_gap = timing.trrd.max((timing.tfaw >> 2) + 1);
    act.addr_wrap = 18;

    let mut write = SequenceStep::simple(IOSAV_WR, slotrank, 0, 0, timing.cwl + timing.twtr + 8);
    write.ranksel_ap = 1;
    write.cmd_executions = 32;
    write.cmd_delay_gap = 20;
    write.data_direction = SSQ_WR;
    write.rowbits = 0;
    write.inc_addr_8 = true;
    write.addr_wrap = 18;

    let mut read = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, timing.trtp.max(8));
    read.ranksel_ap = 1;
    read.cmd_executions = 32;
    read.cmd_delay_gap = 20;
    read.data_direction = SSQ_RD;
    read.rowbits = 0;
    read.inc_addr_8 = true;
    read.addr_wrap = 18;

    let mut pre = SequenceStep::simple(IOSAV_PRE, slotrank, 0, 1 << 10, timing.trp);
    pre.ranksel_ap = 1;
    pre.cmd_delay_gap = 3;

    write_sequence(mchbar_base, channel, &[act, write, read, pre]);
}

pub fn write_memory_test_sequence(mchbar_base: usize, channel: usize, slotrank: u8) {
    let mut act = SequenceStep::simple(IOSAV_ACT, slotrank, 0, 0, 40);
    act.ranksel_ap = 1;
    act.cmd_executions = 4;
    act.cmd_delay_gap = 8;
    act.inc_bank = true;
    act.addr_wrap = 18;

    let mut write = SequenceStep::simple(IOSAV_WR, slotrank, 0, 0, 40);
    write.ranksel_ap = 1;
    write.cmd_executions = 100;
    write.cmd_delay_gap = 4;
    write.data_direction = SSQ_WR;
    write.rowbits = 0;
    write.inc_addr_8 = true;
    write.addr_wrap = 18;

    let mut read = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, 40);
    read.ranksel_ap = 1;
    read.cmd_executions = 100;
    read.cmd_delay_gap = 4;
    read.data_direction = SSQ_RD;
    read.rowbits = 0;
    read.inc_addr_8 = true;
    read.addr_wrap = 18;

    let mut pre = SequenceStep::simple(IOSAV_PRE, slotrank, 0, 1 << 10, 40);
    pre.ranksel_ap = 1;
    pre.cmd_delay_gap = 3;
    pre.addr_wrap = 18;

    write_sequence(mchbar_base, channel, &[act, write, read, pre]);
}

pub fn write_command_training_sequence(
    mchbar_base: usize,
    channel: usize,
    slotrank: u8,
    timing: CommandTrainingTiming,
    address: u16,
) {
    let mut act = SequenceStep::simple(IOSAV_ACT, slotrank, 0, address, timing.trcd);
    act.ranksel_ap = 1;
    act.cmd_executions = 8;
    act.cmd_delay_gap = timing.trrd.max((timing.tfaw >> 2) + 1);
    act.inc_bank = true;
    act.addr_wrap = 18;

    let mut write = SequenceStep::simple(IOSAV_WR, slotrank, 0, 0, timing.cwl + timing.twtr + 8);
    write.ranksel_ap = 1;
    write.cmd_executions = 32;
    write.cmd_delay_gap = 4;
    write.data_direction = SSQ_WR;
    write.rowbits = 0;
    write.inc_addr_8 = true;
    write.addr_wrap = 18;
    write.lfsr_upd = 3;
    write.lfsr_xors = 2;

    let mut read = SequenceStep::simple(IOSAV_RD, slotrank, 0, 0, timing.trtp.max(8));
    read.ranksel_ap = 1;
    read.cmd_executions = 32;
    read.cmd_delay_gap = 4;
    read.data_direction = SSQ_RD;
    read.rowbits = 0;
    read.inc_addr_8 = true;
    read.addr_wrap = 18;
    read.lfsr_upd = 3;
    read.lfsr_xors = 2;

    let mut pre = SequenceStep::simple(IOSAV_PRE, slotrank, 0, 1 << 10, 15);
    pre.ranksel_ap = 1;
    pre.cmd_delay_gap = 4;
    pre.addr_wrap = 18;

    write_sequence(mchbar_base, channel, &[act, write, read, pre]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_mrs_sequence_step_like_coreboot_bitfields() {
        let mut step = SequenceStep::simple(IOSAV_MRS, 2, 1, 0x1234, 12);
        step.ranksel_ap = 1;
        assert_eq!(step.sp_cmd_ctrl(), 0x0001_f000);
        assert_eq!(step.subseq_ctrl(), 0x000c_1001);
        assert_eq!(step.sp_cmd_addr(), 0x0216_1234);
    }

    #[test]
    fn packs_rank_increment_address_update() {
        let step = SequenceStep {
            inc_rank: 1,
            addr_wrap: 20,
            ..SequenceStep::default()
        };
        assert_eq!(step.addr_update(), (1 << 3) | (20 << 5));
    }
}
