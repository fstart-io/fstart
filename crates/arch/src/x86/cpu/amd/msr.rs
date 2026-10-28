//! AMD64 MSRs every AMD family since K8 has: memory routing, MMCONFIG,
//! SMM and the P-state definitions the TSC runs at. Family-specific ones
//! live with their drivers.

use crate::x86::cpu::msr::{Msr, MsrAccess};
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;

register_bitfields![u64,
    pub Syscfg [
        /// Fixed-MTRR RdDram/WrDram bits are honoured.
        MTRR_FIX_DRAM_EN OFFSET(18) NUMBITS(1) [],
        /// Fixed-MTRR RdDram/WrDram bits are accessible.
        MTRR_FIX_DRAM_MOD_EN OFFSET(19) NUMBITS(1) [],
        /// Variable MTRRs route below TOP_MEM to DRAM.
        MTRR_VAR_DRAM_EN OFFSET(20) NUMBITS(1) [],
        TOM2_EN OFFSET(21) NUMBITS(1) [],
        /// Memory between 4 GiB and TOM2 is write-back.
        TOM2_WB OFFSET(22) NUMBITS(1) [],
        /// Secure memory encryption enable.
        SMEE OFFSET(23) NUMBITS(1) [],
        /// SEV-SNP VM permission levels (16-byte RMP entries).
        VMPL_EN OFFSET(25) NUMBITS(1) []
    ],
    pub Hwcr [
        SMM_LOCK OFFSET(0) NUMBITS(1) [],
        /// The TSC counts at the P0 frequency (fixed to 1 from Family 10h).
        TSC_FREQ_SEL OFFSET(24) NUMBITS(1) [],
        /// Core performance boost disable.
        CPB_DIS OFFSET(25) NUMBITS(1) []
    ],
    /// TOP_MEM: DRAM below 4 GiB ends here.
    pub TopMem [
        ADDRESS OFFSET(23) NUMBITS(17) []
    ],
    /// TOM2: DRAM from 4 GiB ends here.
    pub TopMem2 [
        ADDRESS OFFSET(22) NUMBITS(18) []
    ],
    /// MMIO_CFG_BASE: the ECAM window (not an Intel PCIEXBAR).
    pub MmioCfgBase [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// log2 of the bus count.
        BUS_RANGE OFFSET(2) NUMBITS(4) [],
        /// Base address bits [47:20].
        ADDRESS OFFSET(20) NUMBITS(28) []
    ],
    /// PStateCurLim.
    pub PstateCurLim [
        /// The highest P-state the hardware allows now.
        CUR_PSTATE_LIMIT OFFSET(0) NUMBITS(3) [],
        /// The lowest-performance P-state.
        PSTATE_MAX_VAL OFFSET(4) NUMBITS(3) []
    ],
    /// PStateDef, Family 17h and later.
    pub PstateDef [
        CPU_FID OFFSET(0) NUMBITS(8) [],
        CPU_DFS_ID OFFSET(8) NUMBITS(6) [],
        CPU_VID OFFSET(14) NUMBITS(8) [],
        IDD_VALUE OFFSET(22) NUMBITS(8) [],
        IDD_DIV OFFSET(30) NUMBITS(2) [],
        PSTATE_EN OFFSET(63) NUMBITS(1) []
    ],
    pub SmmMask [
        A_VALID OFFSET(0) NUMBITS(1) [],
        T_VALID OFFSET(1) NUMBITS(1) [],
        TM_TYPE_DRAM OFFSET(12) NUMBITS(3) [WriteBack = 6],
        MASK OFFSET(17) NUMBITS(35) []
    ]
];

pub const SYSCFG: Msr<Syscfg::Register> = Msr::new(0xc001_0010);
pub const HWCR: Msr<Hwcr::Register> = Msr::new(0xc001_0015);
pub const TOP_MEM: Msr<TopMem::Register> = Msr::new(0xc001_001a);
pub const TOP_MEM2: Msr<TopMem2::Register> = Msr::new(0xc001_001d);
pub const MMIO_CFG_BASE: Msr<MmioCfgBase::Register> = Msr::new(0xc001_0058);
pub const PSTATE_CURRENT_LIMIT: Msr<PstateCurLim::Register> = Msr::new(0xc001_0061);
/// P-state 0's definition; P-state `n` is `PSTATE_0.offset(n)`.
pub const PSTATE_0: Msr<PstateDef::Register> = Msr::new(0xc001_0064);
pub const SMM_BASE: Msr = Msr::new(0xc001_0111);
pub const SMM_ADDR: Msr = Msr::new(0xc001_0112);
pub const SMM_MASK: Msr<SmmMask::Register> = Msr::new(0xc001_0113);

/// Core MHz of a Family 17h+ P-state definition, `None` if it isn't
/// enabled.
pub fn pstate_mhz(value: u64) -> Option<u32> {
    let pstate = LocalRegisterCopy::<u64, PstateDef::Register>::new(value);
    if !pstate.is_set(PstateDef::PSTATE_EN) {
        return None;
    }
    let fid = pstate.read(PstateDef::CPU_FID) as u32;
    let divisor = pstate.read(PstateDef::CPU_DFS_ID) as u32;
    if divisor == 0 {
        return Some(4000);
    }
    let mhz =
        if (8..=0x1a).contains(&divisor) || divisor > 0x1a && divisor <= 0x3e && divisor & 1 == 0 {
            200 * fid / divisor
        } else {
            25 * fid
        };
    Some(if mhz == 0 { 4000 } else { mhz })
}

/// The TSC frequency of a Family 17h+ CPU: the MHz of the highest P-state
/// the hardware allows (CurPstateLimit). That is P0 on Rome, but not on
/// Picasso at boot, where Linux calibrates the TSC to it as well.
pub fn tsc_mhz(cpu: &impl MsrAccess) -> Option<u32> {
    let highest = cpu
        .reg(PSTATE_CURRENT_LIMIT)
        .read(PstateCurLim::CUR_PSTATE_LIMIT);
    pstate_mhz(cpu.reg(PSTATE_0.offset(highest as u32)).get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pstate_frequencies() {
        assert_eq!(pstate_mhz((1 << 63) | 100 | (8 << 8)), Some(2500));
        assert_eq!(pstate_mhz((1 << 63) | 100 | (10 << 8)), Some(2000));
        assert_eq!(pstate_mhz((1 << 63) | 100), Some(4000));
        assert_eq!(pstate_mhz(100 | (8 << 8)), None);
    }
}
