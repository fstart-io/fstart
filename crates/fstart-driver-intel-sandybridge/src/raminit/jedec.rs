//! DDR3 JEDEC reset and mode-register command sequencing.

use fstart_arch_x86::udelay;
use fstart_services::ServiceError;

use super::iosav::{self, SequenceStep, BROADCAST_CH, IOSAV_MRS, IOSAV_NOP, IOSAV_ZQCS};
use super::mchbar;
use super::mrs;
use super::RaminitState;

const SCHED_CBIT_BASE: usize = 0x4020;
const TC_MR2_SHADOW_BASE: usize = 0x429c;
const MC_INIT_STATE_CH_BASE: usize = 0x42a0;
const MC_INIT_STATE: usize = 0x4ea0;
const MC_INIT_STATE_G: usize = 0x5030;
const RCOMP_TIMER: usize = 0x5084;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

/// Run the Sandy Bridge DDR3 reset and MRS/ZQCL portion of native raminit.
pub fn initialize_dram(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    jedec_reset(mchbar_base, state)?;
    issue_mrs_commands(mchbar_base, state)
}

fn jedec_reset(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    wait_for_rcomp(mchbar_base)?;
    // Coreboot waits for IOSAV DONE or PANIC before asserting DDR reset here.
    iosav::wait_mask(mchbar_base, 0, 0x14)?;

    let mut reg = 0x112;
    mchbar::write32(mchbar_base + MC_INIT_STATE_G, reg);
    mchbar::write32(mchbar_base + MC_INIT_STATE, 0);
    reg |= 2; // DDR reset.
    mchbar::write32(mchbar_base + MC_INIT_STATE_G, reg);

    // Assert DIMM reset signal, wait 200 us, then deassert and wait 500 us.
    mchbar::clrbits32(mchbar_base + MC_INIT_STATE_G, 1 << 1);
    udelay(200);
    mchbar::setbits32(mchbar_base + MC_INIT_STATE_G, 1 << 1);
    udelay(500);

    // Enable DCLK and wait at least 20 ns.
    mchbar::setbits32(mchbar_base + MC_INIT_STATE_G, 1 << 2);
    udelay(1);

    for channel in 0..2 {
        let rankmap = u32::from(state.topology.rankmap[channel]);
        mchbar::write32(mchbar_base + cx(MC_INIT_STATE_CH_BASE, channel), rankmap);
        mchbar::write32(
            mchbar_base + cx(MC_INIT_STATE_CH_BASE, channel),
            (rankmap & !0xf0) | (rankmap << 4),
        );
        write_reset(mchbar_base, state)?;
    }

    Ok(())
}

fn wait_for_rcomp(mchbar_base: usize) -> Result<(), ServiceError> {
    for _ in 0..1_000_000 {
        if (mchbar::read32(mchbar_base + RCOMP_TIMER) & (1 << 16)) != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ServiceError::Timeout)
}

fn write_reset(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    let channel = if state.topology.rankmap[0] != 0 { 0 } else { 1 };
    iosav::wait(mchbar_base, channel)?;
    let slotrank = if (state.topology.rankmap[channel] & 1) != 0 {
        0
    } else {
        2
    };
    iosav::write_zqcs_sequence(mchbar_base, channel, slotrank, 3, 8, 0);
    iosav::run_queue(mchbar_base, channel, 1, 1, true)?;
    iosav::wait(mchbar_base, channel)
}

fn issue_mrs_commands(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        for slotrank in 0..4 {
            if (state.topology.rankmap[channel] & (1 << slotrank)) == 0 {
                continue;
            }
            let regs = state.mode_registers[channel][slotrank];
            write_mrreg(mchbar_base, state, channel, slotrank, 2, regs.mr2)?;
            program_mr2_shadow(mchbar_base, state, channel, slotrank, regs.mr2);
            write_mrreg(mchbar_base, state, channel, slotrank, 3, regs.mr3)?;
            write_mrreg(mchbar_base, state, channel, slotrank, 1, regs.mr1)?;
            write_mrreg(mchbar_base, state, channel, slotrank, 0, regs.mr0)?;
        }
    }

    zqcl_all_ranks(mchbar_base)?;

    // Refresh enable.
    mchbar::setbits32(mchbar_base + MC_INIT_STATE_G, 1 << 3);

    for channel in 0..2 {
        if state.topology.rankmap[channel] == 0 {
            continue;
        }
        mchbar::clrbits32(mchbar_base + cx(SCHED_CBIT_BASE, channel), 1 << 21);
        iosav::wait(mchbar_base, channel)?;
        let slotrank = if (state.topology.rankmap[channel] & 1) != 0 {
            0
        } else {
            2
        };
        iosav::wait(mchbar_base, channel)?;
        iosav::write_zqcs_sequence(mchbar_base, channel, slotrank, 4, 101, 31);
        iosav::run_once_and_wait(mchbar_base, channel, 1)?;
    }

    Ok(())
}

fn write_mrreg(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
    mut reg: u8,
    mut val: u16,
) -> Result<(), ServiceError> {
    iosav::wait(mchbar_base, channel)?;

    if state.topology.rank_mirror[channel][slotrank] {
        (reg, val) = mrs::mirror_mr_address(reg, val);
    }

    let mut step0 = SequenceStep::simple(IOSAV_MRS, slotrank as u8, reg, val, 4);
    step0.cmd_delay_gap = 4;
    let mut step1 = step0;
    step1.ranksel_ap = 1;
    let mut step2 = step0;
    step2.post_ssq_wait = state.timing.tmod as u16;
    let sequence = [step0, step1, step2];
    iosav::write_sequence(mchbar_base, channel, &sequence);
    iosav::run_once_and_wait(mchbar_base, channel, sequence.len())
}

fn program_mr2_shadow(
    mchbar_base: usize,
    state: &RaminitState,
    channel: usize,
    slotrank: usize,
    mr2: u16,
) {
    let addr = mchbar_base + cx(TC_MR2_SHADOW_BASE, channel);
    let mut reg32 = mchbar::read32(addr);
    reg32 &= (3 << 14) | (3 << 6);
    reg32 |= u32::from(mr2) & !(3 << 6);
    if state.topology.rank_mirror[channel][slotrank] {
        reg32 |= 1 << ((slotrank / 2) + 14);
    }
    mchbar::write32(addr, reg32);
}

fn zqcl_all_ranks(mchbar_base: usize) -> Result<(), ServiceError> {
    let mut nop = SequenceStep::simple(IOSAV_NOP & !(0xff << 8), 0, 0, 2, 15);
    nop.cmd_delay_gap = 4;
    let mut zqcl = SequenceStep::simple(IOSAV_ZQCS, 0, 0, 1 << 10, 400);
    zqcl.ranksel_ap = 1;
    zqcl.cmd_delay_gap = 4;
    zqcl.inc_rank = 1;
    zqcl.addr_wrap = 20;
    let sequence = [nop, zqcl];
    iosav::write_sequence(mchbar_base, BROADCAST_CH, &sequence);
    iosav::run_queue(mchbar_base, BROADCAST_CH, sequence.len(), 4, false)?;
    iosav::wait(mchbar_base, 0)?;
    iosav::wait(mchbar_base, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zqcl_sequence_uses_broadcast_safe_rank_increment_shape() {
        let mut zqcl = SequenceStep::simple(IOSAV_ZQCS, 0, 0, 1 << 10, 400);
        zqcl.ranksel_ap = 1;
        zqcl.inc_rank = 1;
        zqcl.addr_wrap = 20;
        assert_eq!(zqcl.command, IOSAV_ZQCS);
        assert_eq!(zqcl.address, 1 << 10);
        assert_eq!(zqcl.ranksel_ap, 1);
        assert_eq!(zqcl.inc_rank, 1);
        assert_eq!(zqcl.addr_wrap, 20);
    }
}
