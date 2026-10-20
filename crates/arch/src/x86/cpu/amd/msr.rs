//! AMD64 MSRs every AMD family since K8 has: memory routing, MMCONFIG and
//! SMM. Family-specific ones live with their drivers.

use crate::x86::cpu::msr::Msr;
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
        SMEE OFFSET(23) NUMBITS(1) []
    ],
    pub Hwcr [
        SMM_LOCK OFFSET(0) NUMBITS(1) []
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
pub const SMM_BASE: Msr = Msr::new(0xc001_0111);
pub const SMM_ADDR: Msr = Msr::new(0xc001_0112);
pub const SMM_MASK: Msr<SmmMask::Register> = Msr::new(0xc001_0113);
