//! D-unit (memory controller) registers, IOSF port 0x01 (channel 0), and
//! the JEDEC commands the D-unit sends to the DRAM.
//!
//! Field layout from coreboot `soc/intel/baytrail/include/soc/dunit_regs.h`,
//! with the DRP width/density encodings confirmed against the vendor MRC
//! (`ProgDraDrb`).

use crate::baytrail::iosf::{DUNIT_CH0, IosfReg};
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// Rank population and geometry.
    pub DRP [
        RANK0_EN OFFSET(0) NUMBITS(1) [],
        RANK1_EN OFFSET(1) NUMBITS(1) [],
        RANK2_EN OFFSET(2) NUMBITS(1) [],
        RANK3_EN OFFSET(3) NUMBITS(1) [],
        RANK_EN OFFSET(0) NUMBITS(4) [],
        /// Device width: x8 = 0, x16 = 1, x32 = 2.
        DIMM0_WIDTH OFFSET(4) NUMBITS(2) [],
        /// Device density: 1 Gb = 0, 2 Gb = 1, 4 Gb = 2, 8 Gb = 3.
        DIMM0_DENSITY OFFSET(6) NUMBITS(2) [],
        DIMM1_WIDTH OFFSET(9) NUMBITS(2) [],
        DIMM1_DENSITY OFFSET(11) NUMBITS(2) [],
        /// Rank select interleave.
        RSIEN OFFSET(14) NUMBITS(1) [],
        DIMM_FLIP OFFSET(16) NUMBITS(1) [],
        RANK_REMAP OFFSET(17) NUMBITS(1) [],
        CKE_COPY OFFSET(18) NUMBITS(1) [],
        DIMM0_MIRROR OFFSET(20) NUMBITS(1) [],
        DIMM1_MIRROR OFFSET(21) NUMBITS(1) [],
        /// 0 = DDR3, 1 = LPDDR3.
        DRAM_TYPE OFFSET(22) NUMBITS(1) [],
        EN_LPDDR3 OFFSET(23) NUMBITS(1) []
    ],
    /// Frequency, CL, tRCD, tRP.
    pub DTR0 [
        DRAM_FREQ OFFSET(0) NUMBITS(2) [],
        /// tRP - 5.
        T_RP OFFSET(4) NUMBITS(4) [],
        /// tRCD - 5.
        T_RCD OFFSET(8) NUMBITS(4) [],
        /// CL - 5.
        T_CL OFFSET(12) NUMBITS(3) [],
        T_XS OFFSET(16) NUMBITS(1) [],
        T_XSDLL OFFSET(18) NUMBITS(1) [],
        T_ZQCS OFFSET(20) NUMBITS(1) [],
        T_ZQCL OFFSET(22) NUMBITS(1) [],
        PMEDLY OFFSET(24) NUMBITS(2) [],
        CKEDLY OFFSET(28) NUMBITS(4) []
    ],
    /// WCL, command rate, write/activate timings.
    pub DTR1 [
        /// WCL - 3.
        T_WCL OFFSET(0) NUMBITS(3) [],
        /// Command transport: 0 = 1N.
        T_CMD OFFSET(4) NUMBITS(2) [],
        T_WTP OFFSET(8) NUMBITS(4) [],
        T_CCD OFFSET(12) NUMBITS(2) [],
        T_FAW OFFSET(16) NUMBITS(4) [],
        T_RAS OFFSET(20) NUMBITS(4) [],
        T_RRD OFFSET(24) NUMBITS(2) [],
        T_RTP OFFSET(28) NUMBITS(3) []
    ],
    /// Cross-rank and cross-DIMM turnarounds.
    pub DTR2 [
        T_RRDR OFFSET(0) NUMBITS(3) [],
        T_RRDD OFFSET(4) NUMBITS(3) [],
        T_WWDR OFFSET(8) NUMBITS(3) [],
        T_WWDD OFFSET(12) NUMBITS(3) [],
        T_RWDR OFFSET(16) NUMBITS(4) [],
        T_RWDD OFFSET(21) NUMBITS(4) []
    ],
    /// Write-to-read turnarounds, power-down, derating.
    pub DTR3 [
        T_WRDR OFFSET(0) NUMBITS(3) [],
        T_WRDD OFFSET(4) NUMBITS(3) [],
        T_RWSR OFFSET(8) NUMBITS(4) [],
        T_WRSR OFFSET(13) NUMBITS(4) [],
        T_XP OFFSET(22) NUMBITS(2) [],
        PWD_DLY OFFSET(24) NUMBITS(4) [],
        EN_DE_RATE OFFSET(28) NUMBITS(1) [],
        DE_RATE_OVR OFFSET(29) NUMBITS(1) [],
        DE_RATE_STAT OFFSET(30) NUMBITS(1) []
    ],
    /// ODT timing.
    pub DTR4 [
        WR_ODT_STRT OFFSET(0) NUMBITS(2) [],
        WR_ODT_STOP OFFSET(4) NUMBITS(3) [],
        RD_ODT_STRT OFFSET(8) NUMBITS(3) [],
        RD_ODT_STOP OFFSET(12) NUMBITS(3) [],
        TRGSTR_DIS OFFSET(16) NUMBITS(1) [],
        RD_ODT_DIS OFFSET(17) NUMBITS(1) [],
        WRB_ODT_DIS OFFSET(18) NUMBITS(1) []
    ],
    /// Power management: self-refresh, clock gating.
    pub DPMC0 [
        SR_ENTRY_DLY OFFSET(0) NUMBITS(8) [],
        PM_OPCODE OFFSET(8) NUMBITS(5) [],
        PCLSTO OFFSET(16) NUMBITS(3) [],
        PCLSWKOK OFFSET(20) NUMBITS(1) [],
        PREAPWDEN OFFSET(21) NUMBITS(1) [],
        DYNSREN OFFSET(23) NUMBITS(1) [],
        CLKGTDIS OFFSET(24) NUMBITS(1) [],
        DISPWRDN OFFSET(25) NUMBITS(1) [],
        /// 0 = fixed BL8, 1 = on-the-fly burst length.
        BLMODE OFFSET(26) NUMBITS(1) [],
        PWRGATEDIS OFFSET(27) NUMBITS(1) [],
        REUTCLKGTDIS OFFSET(28) NUMBITS(1) [],
        ENPHYCLKGATE OFFSET(29) NUMBITS(1) [],
        ENCKTRI OFFSET(30) NUMBITS(1) [],
        ENCORECLKGATE OFFSET(31) NUMBITS(1) []
    ],
    /// CMD and chip-select tristate.
    pub DPMC1 [ CS_TRIST OFFSET(0) NUMBITS(1) [], CMD_TRIST OFFSET(4) NUMBITS(2) [] ],
    /// Refresh control and watermarks.
    pub DRFC [
        REFWMLO OFFSET(0) NUMBITS(4) [],
        REFWMHI OFFSET(4) NUMBITS(4) [],
        REFWMPNC OFFSET(8) NUMBITS(4) [],
        TREFI OFFSET(12) NUMBITS(3) [],
        REFCNTMAX OFFSET(16) NUMBITS(2) [],
        REFSKEWDIS OFFSET(20) NUMBITS(1) [],
        REFDBTCLR OFFSET(21) NUMBITS(1) [],
        CU_REF_RATE OFFSET(24) NUMBITS(3) []
    ],
    /// Scheduler.
    pub DSCH [
        OOOAGETRH OFFSET(0) NUMBITS(5) [],
        OOODIS OFFSET(8) NUMBITS(1) [],
        OOOST3DIS OFFSET(9) NUMBITS(1) [],
        NEWBYPDIS OFFSET(12) NUMBITS(1) [],
        IPREQMAX OFFSET(16) NUMBITS(3) []
    ],
    /// ZQ calibration and temperature polling.
    pub DCAL [
        ZQCINT OFFSET(8) NUMBITS(3) [],
        SRXZQCL OFFSET(12) NUMBITS(2) [],
        ZQCAL_TYPE OFFSET(14) NUMBITS(1) [],
        ZQCAL_START OFFSET(15) NUMBITS(1) [],
        TQPOLL_START OFFSET(16) NUMBITS(1) [],
        TQPOLL_RS OFFSET(17) NUMBITS(2) [],
        TQPOLL_EN OFFSET(20) NUMBITS(1) [],
        TQPOLL_PER OFFSET(21) NUMBITS(3) [],
        MRR_DATA OFFSET(24) NUMBITS(8) []
    ],
    /// Rank mode control: CKE and ODT pin overrides.
    pub DRMC [
        CKE_VAL OFFSET(0) NUMBITS(4) [],
        CKE_MODE OFFSET(4) NUMBITS(1) [],
        ODT_VAL OFFSET(8) NUMBITS(4) [],
        ODT_MODE OFFSET(12) NUMBITS(1) [],
        /// Cold-reset DRAM wake.
        COLD_WAKE OFFSET(16) NUMBITS(1) []
    ],
    pub PMSTS [ SELF_REFRESH OFFSET(0) NUMBITS(1) [] ],
    /// Configuration and lock bits.
    pub DCO [
        DRPLOCK OFFSET(0) NUMBITS(1) [],
        REUTLOCK OFFSET(8) NUMBITS(1) [],
        PMICTL OFFSET(28) NUMBITS(1) [],
        PMIDIS OFFSET(29) NUMBITS(1) [],
        DIOIC OFFSET(30) NUMBITS(1) [],
        /// Initialization complete.
        IC OFFSET(31) NUMBITS(1) []
    ],
    pub BONUS0 [ SLOWPDXEN OFFSET(1) NUMBITS(1) [], LPDDRCMDTRI OFFSET(7) NUMBITS(1) [] ],
    pub SCRMSEED [ DISABLE OFFSET(31) NUMBITS(1) [] ],

    /// The D-unit JEDEC command word (IOSF opcode 0x68). The low bits
    /// carry the command and bank, MA[15:0] from bit 6, the rank from
    /// bit 22.
    pub JEDEC [
        COMMAND OFFSET(0) NUMBITS(3) [
            Mrs = 0,
            Precharge = 2,
            Zqcal = 6,
            Nop = 7
        ],
        BANK OFFSET(3) NUMBITS(3) [],
        ADDRESS OFFSET(6) NUMBITS(16) [],
        RANK OFFSET(22) NUMBITS(2) []
    ],
    /// MR0 in the address bits of [`JEDEC`].
    pub MR0 [
        /// 1 = on-the-fly BL8/BC4.
        BL OFFSET(6) NUMBITS(2) [],
        CL_LOW OFFSET(8) NUMBITS(1) [],
        RBT OFFSET(9) NUMBITS(1) [],
        CL_HIGH OFFSET(10) NUMBITS(3) [],
        TM OFFSET(13) NUMBITS(1) [],
        DLL_RESET OFFSET(14) NUMBITS(1) [],
        WR OFFSET(15) NUMBITS(3) [],
        PPD OFFSET(18) NUMBITS(1) []
    ],
    /// MR1 in the address bits of [`JEDEC`].
    pub MR1 [
        /// 0 = DLL enabled.
        DLL_DISABLE OFFSET(6) NUMBITS(1) [],
        DIC0 OFFSET(7) NUMBITS(1) [],
        RTT_NOM0 OFFSET(8) NUMBITS(1) [],
        AL OFFSET(9) NUMBITS(2) [],
        DIC1 OFFSET(11) NUMBITS(1) [],
        RTT_NOM1 OFFSET(12) NUMBITS(1) [],
        WL_EN OFFSET(13) NUMBITS(1) [],
        RTT_NOM2 OFFSET(15) NUMBITS(1) [],
        TDQS OFFSET(17) NUMBITS(1) [],
        QOFF OFFSET(18) NUMBITS(1) []
    ],
    /// MR2 in the address bits of [`JEDEC`].
    pub MR2 [
        PASR OFFSET(6) NUMBITS(3) [],
        /// CAS write latency - 5.
        CWL OFFSET(9) NUMBITS(3) [],
        ASR OFFSET(12) NUMBITS(1) [],
        SRT OFFSET(13) NUMBITS(1) [],
        /// Dynamic ODT: 2 = RZQ/2 (120 ohm).
        RTT_WR OFFSET(15) NUMBITS(2) []
    ],
    /// MR3 in the address bits of [`JEDEC`].
    pub MR3 [ MPR_LOC OFFSET(6) NUMBITS(2) [], MPR OFFSET(8) NUMBITS(1) [] ]
];

pub const DRP_REG: IosfReg<DRP::Register> = DUNIT_CH0.reg(0x00);
pub const DTR0_REG: IosfReg<DTR0::Register> = DUNIT_CH0.reg(0x01);
pub const DTR1_REG: IosfReg<DTR1::Register> = DUNIT_CH0.reg(0x02);
pub const DTR2_REG: IosfReg<DTR2::Register> = DUNIT_CH0.reg(0x03);
pub const DTR3_REG: IosfReg<DTR3::Register> = DUNIT_CH0.reg(0x04);
pub const DTR4_REG: IosfReg<DTR4::Register> = DUNIT_CH0.reg(0x05);
pub const DPMC0_REG: IosfReg<DPMC0::Register> = DUNIT_CH0.reg(0x06);
pub const DPMC1_REG: IosfReg<DPMC1::Register> = DUNIT_CH0.reg(0x07);
pub const DRFC_REG: IosfReg<DRFC::Register> = DUNIT_CH0.reg(0x08);
pub const DSCH_REG: IosfReg<DSCH::Register> = DUNIT_CH0.reg(0x09);
pub const DCAL_REG: IosfReg<DCAL::Register> = DUNIT_CH0.reg(0x0a);
pub const DRMC_REG: IosfReg<DRMC::Register> = DUNIT_CH0.reg(0x0b);
pub const PMSTS_REG: IosfReg<PMSTS::Register> = DUNIT_CH0.reg(0x0c);
pub const DCO_REG: IosfReg<DCO::Register> = DUNIT_CH0.reg(0x0f);
pub const BONUS0_REG: IosfReg<BONUS0::Register> = DUNIT_CH0.reg(0x50);
pub const SCRMSEED_REG: IosfReg<SCRMSEED::Register> = DUNIT_CH0.reg(0x80);
pub const SCRMLO: IosfReg = DUNIT_CH0.reg(0x81);
pub const SCRMHI: IosfReg = DUNIT_CH0.reg(0x82);
