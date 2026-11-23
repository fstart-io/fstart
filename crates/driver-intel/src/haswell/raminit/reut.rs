//! Rank-selective CADB commands. The buffer's write pointer advances on
//! *each transaction*, so two dword stores cannot replace one qword store.
use super::{Error, MchBar, mc, spd::Topology};
use crate::haswell::registers::*;

pub(super) fn write_qword(mch: MchBar, off: u32, value: u64) {
    mch.write64(off, value);
}

pub(super) fn read_qword(mch: MchBar, off: u32) -> u64 {
    mch.read64(off)
}
fn mirror(word: u64) -> u64 {
    let swap = (word ^ (word >> 1)) & ((1 << 24) | (1 << 7) | (1 << 5) | (1 << 3));
    word ^ swap ^ (swap << 1)
}

struct Command {
    opcode: u64,
    bank: u8,
    values: [u16; 4],
    gap: u32,
}
fn issue(mch: MchBar, t: &Topology, ch: usize, rankmask: u8, cmd: Command) -> Result<(), Error> {
    use REUT_PAT_CADB_PROG as p;
    let mask = t.rankmap[ch] & rankmask;
    if mask == 0 {
        return Err(Error::Reut);
    }
    mch.clrbits32(0x4c30, MCSCHEDS_DFT_MISC::DDR_QUALIFIER.val(3).value);
    mch.write8(mc(0x41bc, ch), 0);
    for (rank, val) in cmd.values.iter().enumerate() {
        if mask & (1 << rank) == 0 {
            continue;
        }
        let word = (p::ADDR.val((*val).into())
            + p::BANK.val(cmd.bank.into())
            + p::CS.val(!(1 << rank))
            + p::CMD.val(cmd.opcode)
            + p::CKE.val(15))
        .value;
        write_qword(
            mch,
            mc(0x41c0, ch),
            if t.mirrored[ch] & (1 << rank) != 0 {
                mirror(word)
            } else {
                word
            },
        );
    }
    mch.write32(
        mc(0x419c, ch),
        (REUT_PAT_CADB_MRS::DELAY_GAP.val(cmd.gap)
            + REUT_PAT_CADB_MRS::END_PTR.val(mask.count_ones() - 1))
        .value,
    );
    let cfg = 0x48a8 + 8 * ch as u32;
    let ctl = 0x48b8 + 4 * ch as u32;
    let save = mch.read32(cfg);
    let v = (REUT_SEQ_CFG::GLOBAL_CONTROL::CLEAR + REUT_SEQ_CFG::INITIALIZATION_MODE.val(2))
        .modify(u64::from(save)) as u32;
    mch.write32(cfg, v);
    mch.write32(
        ctl,
        (REUT_SEQ_CTL::START_TEST::SET + REUT_SEQ_CTL::CLEAR_ERRORS::SET).value,
    );
    let mut error = false;
    let result = super::poll(
        || {
            let status = mch.read32(0x4804);
            // As in coreboot's reut_write_cadb_cmd, an error ends the wait:
            // it is a REUT error for this channel, not a stuck controller.
            error = REUT_GLOBAL_ERR::CH_ERROR.read(status) & (1 << ch) != 0;
            error || REUT_GLOBAL_ERR::CH_TEST_DONE.read(status) & (1 << ch) != 0
        },
        100_000,
    );
    mch.write32(ctl, REUT_SEQ_CTL::CLEAR_ERRORS::SET.value);
    mch.write32(cfg, save);
    result?;
    if error { Err(Error::Reut) } else { Ok(()) }
}
pub fn mrs(mch: MchBar, t: &Topology, ch: usize, ranks: u8, mr: u8, val: u16) -> Result<(), Error> {
    issue(
        mch,
        t,
        ch,
        ranks,
        Command {
            opcode: 0,
            bank: mr,
            values: [val; 4],
            gap: 3,
        },
    )
}
pub fn mrs_all(mch: MchBar, t: &Topology, ch: usize, mr: u8, slots: [u16; 2]) -> Result<(), Error> {
    issue(
        mch,
        t,
        ch,
        15,
        Command {
            opcode: 0,
            bank: mr,
            values: [slots[0], slots[0], slots[1], slots[1]],
            gap: 3,
        },
    )
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Calibration {
    Init,
    Long,
    Short,
    Reset,
}
pub fn zq(mch: MchBar, t: &Topology, chanmask: u8, kind: Calibration) -> Result<(), Error> {
    let long = matches!(kind, Calibration::Init | Calibration::Long);
    let gap = if kind == Calibration::Short && t.dpc.contains(&0) {
        7
    } else {
        1
    };
    let result = t
        .channels()
        .filter(|&ch| chanmask & (1 << ch) != 0)
        .try_for_each(|ch| {
            issue(
                mch,
                t,
                ch,
                15,
                Command {
                    opcode: 6,
                    bank: 0,
                    values: [if long { 1 << 10 } else { 0 }; 4],
                    gap,
                },
            )
        });
    if long {
        fstart_arch::x86::udelay(1);
    }
    result
}
pub fn bitlane_errors(mch: MchBar, ch: usize) -> [u8; 9] {
    let mut bytes = [0; 9];
    for i in [0, 4] {
        bytes[i..i + 4].copy_from_slice(&mch.read32(mc(0x40e0 + i as u32, ch)).to_le_bytes());
    }
    bytes[8] = mch.read8(mc(0x40e8, ch));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadb_mirrors_address_and_bank_only() {
        let word = 0xf000_0e00_0100_00a8;
        assert_eq!(mirror(word), 0xf000_0e00_0200_0150);
        assert_eq!(mirror(mirror(word)), word);
    }
}
