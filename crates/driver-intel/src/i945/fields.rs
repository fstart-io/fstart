//! i945 MCHBAR/EPBAR/DMIBAR register field definitions.
//!
//! `register_bitfields!` catalog for the read-modify-write sites in
//! [`super`](crate::i945) and [`super::raminit`](crate::i945::raminit).
//! Raw offsets stay as `u32` constants at the use sites (tree-consistent
//! with the gm965/pineview drivers); these definitions name the FIELDS so
//! review reads intent instead of shifts. Field widths are chosen so every
//! value the flow computes fits without truncation:
//!
//! - enums, single bits, and fixed table constants: exact widths;
//! - computed timing fields: widths covering the validated maxima
//!   (`cas` 3-5, `twr` 3-5, `trp`/`trcd`/`tras` capped by `derive_timings`);
//! - anything wider or unclear stays a numeric shift at the use site
//!   (e.g. DRT1 `TRFC`, whose DDR2 range can exceed 6 bits).

use tock_registers::register_bitfields;

register_bitfields![u32,
    /// DRAM Controller Control (MCHBAR DCC, 0x200).
    pub DCC_REG [
        COMMAND OFFSET(16) NUMBITS(3) [
            Nop = 1,
            Precharge = 2,
            Mrs = 3,
            Emrs = 4,
            Cbr = 6,
            Normal = 7
        ],
        INIT_COMPLETE OFFSET(19) NUMBITS(1) [],
        XOR_EN OFFSET(9) NUMBITS(1) [],
        XOR_DIS OFFSET(10) NUMBITS(1) []
    ],

    /// DRAM Timing 0 (CxDRT0): read-to-write and tRD assembly.
    pub DRT0_REG [
        /// B2B write precharge, same bank: CL-1 + BL/2 + tWR (max ~13).
        B2B_W_PCHG OFFSET(28) NUMBITS(4) [],
        /// B2B write-to-read spacing: CL-1 + BL/2 + tWTR (max ~11).
        B2B_W2R OFFSET(24) NUMBITS(4) [],
        /// Fixed per coreboot: (1<<22)|(3<<20)|(1<<18).
        FIXED OFFSET(16) NUMBITS(8) [],
        /// tRD delay on the FSB in mclk domain: CAS + FSB bump (max ~8).
        TRD OFFSET(11) NUMBITS(4) [],
        /// Write auto-precharge, same bank: + tRP (max ~19).
        W_AUTO_PCHG OFFSET(4) NUMBITS(5) [],
        /// Read auto-precharge to activate: fixed 8.
        R_AP_TO_ACT OFFSET(0) NUMBITS(8) []
    ],

    /// DRAM Timing 1 (CxDRT1): CAS/RAS assembly.
    pub DRT1_REG [
        /// Page-size selector: 0, 1, or 2.
        PAGE OFFSET(30) NUMBITS(2) [],
        /// Read-to-precharge at 667 MHz.
        TRTP_667 OFFSET(28) NUMBITS(1) [],
        /// Receive-enable coarse delay (training).
        RCVEN_COARSE OFFSET(24) NUMBITS(4) [],
        /// Activate to precharge: tras (capped at 0x18).
        TRAS OFFSET(19) NUMBITS(8) [],
        /// CAS latency encoding (cas_table value 0-3).
        CAS OFFSET(8) NUMBITS(3) [],
        /// RAS-to-CAS delay: trcd - 2 (max 4).
        TRCD OFFSET(4) NUMBITS(3) [],
        /// RAS precharge: trp - 2 (max 4).
        TRP OFFSET(0) NUMBITS(3) []
    ],

    /// DRAM Timing 3 (CxDRT3): refresh-window assembly.
    pub DRT3_REG [
        /// High byte of the 1us field (table constant).
        US_HI OFFSET(16) NUMBITS(8) [],
        /// Low nibble-ish of the 1us field (table constant).
        US_LO OFFSET(10) NUMBITS(6) [],
        /// 788ns minus tRFC, masked to 9 bits at the use site.
        REF_MINUS_TRFC OFFSET(0) NUMBITS(9) []
    ],

    /// DRAM Controller 0 (CxDRC0).
    pub DRC0_REG [
        /// Single-channel-0 second-DIMM present path.
        SC1_SECOND_DIMM OFFSET(15) NUMBITS(1) [],
        /// Refresh rate: 1 = 15.6us, 2 = 7.8us.
        REFRESH OFFSET(8) NUMBITS(3) [],
        /// Burst length 8.
        BURST8 OFFSET(2) NUMBITS(1) []
    ],

    /// DRAM Controller 1 (CxDRC1).
    pub DRC1_REG [
        /// Channel XOR mode (enhanced addressing).
        XOR_MODE OFFSET(24) NUMBITS(8) [],
        /// CKE control pair (programmed to 3 by CKE/PM setup).
        CKE OFFSET(11) NUMBITS(2) [],
        /// Channel IO buffer enable.
        IO_BUF_EN OFFSET(8) NUMBITS(1) [],
        /// Receive-enable toggle strobe.
        RCVEN_TOGGLE OFFSET(6) NUMBITS(1) []
    ],

    /// Write Control (MCHBAR WCC, 0x218).
    pub WCC_REG [
        WRITE_DIS OFFSET(29) NUMBITS(3) [],
        READ_DIS OFFSET(25) NUMBITS(2) [],
        POSTED_WRITE OFFSET(10) NUMBITS(1) []
    ],

    /// Global RCOMP Control (MCHBAR GBRCOMPCTL, 0x400).
    pub GBRCOMPCTL_REG [
        RCOMP_DONE OFFSET(29) NUMBITS(1) [],
        RCOMP_CFG27 OFFSET(27) NUMBITS(1) [],
        RCOMP_CFG26 OFFSET(26) NUMBITS(1) [],
        RCOMP_CFG21 OFFSET(21) NUMBITS(1) [],
        PERIODIC_DIS OFFSET(23) NUMBITS(1) [],
        RCOMP_FORCE OFFSET(8) NUMBITS(1) [],
        RCOMP_ALT OFFSET(5) NUMBITS(2) [],
        RCOMP_DONE_FLAG OFFSET(10) NUMBITS(1) [],
        RCOMP_CFG2 OFFSET(2) NUMBITS(2) [],
        RCOMP_EN OFFSET(0) NUMBITS(2) []
    ],

    /// Scheduler Buffer/OCC config (MCHBAR SBOCC, 0x238).
    pub SBOCC_REG [
        OCC_TIMER OFFSET(8) NUMBITS(16) [],
        OCC_EN OFFSET(0) NUMBITS(1) []
    ],

    /// ODT Control (MCHBAR ODTC, 0x284).
    pub ODTC_REG [
        RCOMP_FORCE_ODT OFFSET(28) NUMBITS(1) [],
        ODT_MODE OFFSET(16) NUMBITS(2) [],
        ODT_EN OFFSET(14) NUMBITS(1) [],
        ODT_REF OFFSET(6) NUMBITS(1) []
    ],

    /// Receive-Enable Mode (MCHBAR RCVENMT, 0x2f8).
    pub RCVENMT_REG [
        CH0_EN OFFSET(11) NUMBITS(1) [],
        CH0_MED OFFSET(9) NUMBITS(1) [],
        CH0_MEDIUM OFFSET(2) NUMBITS(2) [],
        CH1_MEDIUM OFFSET(0) NUMBITS(2) []
    ],

    /// DRAM Test (MCHBAR DRTST, 0x2a8).
    pub DRTST_REG [
        CH0_EMPTY OFFSET(31) NUMBITS(1) [],
        CH1_EMPTY OFFSET(30) NUMBITS(1) [],
        CH1_IO0 OFFSET(9) NUMBITS(1) [],
        CH1_IO1 OFFSET(8) NUMBITS(1) [],
        CH0_IO0 OFFSET(7) NUMBITS(1) [],
        CH0_IO1 OFFSET(5) NUMBITS(1) [],
        TEST_EN OFFSET(6) NUMBITS(1) [],
        TEST_MODE OFFSET(4) NUMBITS(1) [],
        IO_EN OFFSET(3) NUMBITS(1) [],
        IO_MODE OFFSET(2) NUMBITS(1) []
    ],

    /// Egress VC1 control (EPBAR EPVC1RCTL, 0x20).
    pub EPVC1RCTL_REG [
        VC_EN OFFSET(31) NUMBITS(1) [],
        VC_ID OFFSET(24) NUMBITS(3) [],
        ARB_LOAD OFFSET(16) NUMBITS(1) [],
        VC_ARB OFFSET(7) NUMBITS(1) [],
        TC_VC_MAP OFFSET(1) NUMBITS(7) [],
        TC0_VC OFFSET(0) NUMBITS(1) []
    ],

    /// DMI VC1 control (DMIBAR DMIVC1RCTL, 0x20).
    pub DMIVC1RCTL_REG [
        VC_EN OFFSET(31) NUMBITS(1) [],
        VC_ID OFFSET(24) NUMBITS(3) [],
        VC_ARB OFFSET(7) NUMBITS(1) [],
        TC_VC_MAP OFFSET(1) NUMBITS(7) [],
        TC0_VC OFFSET(0) NUMBITS(1) []
    ],

    /// DMI link capabilities (DMIBAR DMILCAP, 0x84).
    pub DMILCAP_REG [
        L1_EXIT_LAT OFFSET(15) NUMBITS(3) [],
        L0S_EXIT_LAT OFFSET(12) NUMBITS(3) []
    ],

    /// DMI chip control (DMIBAR DMICC, 0x208).
    pub DMICC_REG [
        VC1_EN OFFSET(20) NUMBITS(2) [],
        VC0_EN OFFSET(0) NUMBITS(2) []
    ],

    /// FSB power management 3 (MCHBAR FSBPMC3, 0x40).
    pub FSBPMC3_REG [
        DIS_QPML OFFSET(29) NUMBITS(1) [],
        PM_ENABLE OFFSET(21) NUMBITS(1) [],
        FAST_DISPATCH OFFSET(19) NUMBITS(1) [],
        GM_ERRATA OFFSET(13) NUMBITS(1) [],
        PROC_SIDE OFFSET(2) NUMBITS(1) [],
        FAST_DISPATCH_EN OFFSET(1) NUMBITS(1) []
    ],

    /// FSB power management 4 (MCHBAR FSBPMC4, 0x44).
    pub FSBPMC4_REG [
        PM_MODE OFFSET(24) NUMBITS(2) [],
        PM_EN21 OFFSET(21) NUMBITS(1) [],
        PM_EN5 OFFSET(5) NUMBITS(1) [],
        PM_POLARITY OFFSET(4) NUMBITS(1) []
    ],

    /// Power management config (MCHBAR PMCFG, 0xf10).
    pub PMCFG_REG [
        PM_MODE OFFSET(17) NUMBITS(2) [],
        PM_EN OFFSET(4) NUMBITS(1) []
    ],

    /// Clock control for power, low byte programmed numerically; the PM
    /// divider lives in the u16 block below.

    /// C-state timing (MCHBAR C2C3TT/C3C4TT) is programmed with whole-word
    /// frequency constants at the use site; no fields needed.

    /// DMI link control (DMIBAR DMILCTL, 0x88): ASPM enable.
    pub DMILCTL_REG [
        ASPM_CTRL OFFSET(0) NUMBITS(2) []
    ],

    /// DMI control 1 (DMIBAR DMICTL1, 0xf0).
    pub DMICTL1_REG [
        MISC_CTRL OFFSET(24) NUMBITS(2) []
    ],

    /// DMI control 2 (DMIBAR DMICTL2, 0xfc).
    pub DMICTL2_REG [
        MISC_CTRL31 OFFSET(31) NUMBITS(1) []
    ],

    /// DMI DRCCFG (DMIBAR DMIDRCCFG, 0xeb4).
    pub DMIDRCCFG_REG [
        MISC_CTRL31 OFFSET(31) NUMBITS(1) []
    ],

    /// Receive-Enable Phase Control (MCHBAR REPC, 0x2e0).
    pub REPC_REG [
        RCVEN_EN OFFSET(0) NUMBITS(1) []
    ],

    /// SM Voltage Reference Control (MCHBAR SMVREFC, 0x2a0).
    pub SMVREFC_REG [
        SMVREF_EN OFFSET(6) NUMBITS(1) []
    ],

    /// SMS RCOMP Control (MCHBAR SMSRCTL, 0x408).
    pub SMSRCTL_REG [
        SM_RCOMP_EN OFFSET(0) NUMBITS(1) []
    ],

    /// ECO strapping (MCHBAR ECO, 0xffc).
    pub ECO_REG [
        ECO_BIT16 OFFSET(16) NUMBITS(1) []
    ],

    /// Misc BAR 0x0b00 is accessed 8-bit wide; its field lives in the
    /// u8 block below.

    /// Misc BAR 0x0b18 (MCHBAR).
    pub MISC_B18_REG [
        MISC_CTRL21 OFFSET(21) NUMBITS(1) []
    ],

    /// Sleep Control (MCHBAR SLPCTL, 0x90, rev A0).
    pub SLPCTL_REG [
        SLPCTL_B8 OFFSET(8) NUMBITS(1) []
    ],

    /// Write-DLL Bypass Mode (MCHBAR WDLLBYPMODE) stays numeric: the
    /// programmed bit pattern has no documented field split.

    /// DRAM Controller power-down (MCHBAR CxDMC, 0x164).
    pub DMC_REG [
        PWR_DOWN_ACPI OFFSET(24) NUMBITS(1) []
    ],

    /// Egress port VC capability 1 (EPBAR EPPVCCAP1, 0x04).
    pub EPPVCCAP1_REG [
        VC_COUNT OFFSET(0) NUMBITS(3) []
    ],

    /// Egress VC1 resource capability (EPBAR EPVC1RCAP, 0x1c).
    pub EPVC1RCAP_REG [
        VC1_MTS OFFSET(16) NUMBITS(7) []
    ],

    /// DMI VC capability 1 (DMIBAR DMIPVCCAP1, 0x04).
    pub DMIPVCCAP1_REG [
        VC_COUNT OFFSET(0) NUMBITS(3) []
    ],

    /// FSB snoop control (MCHBAR FSBSNPCTL, 0x48).
    pub FSBSNPCTL_REG [
        SNP_MODE OFFSET(2) NUMBITS(8) []
    ],

    /// Egress port element self-description (EPBAR EPESD, 0x44).
    pub EPESD_REG [
        COMP_ID OFFSET(16) NUMBITS(8) []
    ],

    /// DRAM Timing 2 (CxDRT2): idle timers in the low byte.
    pub DRT2_REG [
        CKE_IDLE OFFSET(4) NUMBITS(2) [],
        TIMER_LO OFFSET(0) NUMBITS(8) []
    ]
];

register_bitfields![u16,
    /// Clock control for power, PM divider (MCHBAR CPCTL, 0xc16).
    pub CPCTL_REG [
        PM_DIV OFFSET(11) NUMBITS(3) [],
        PM_UPDATE OFFSET(10) NUMBITS(1) []
    ],
    /// DQ Sense Max Threshold (MCHBAR DQSMT, 0x2f4).
    /// Programmed bit-exact: clear {13,12,10,3..0}, set {13,3,2}.
    pub DQSMT_REG [
        DQSMT_B13 OFFSET(13) NUMBITS(1) [],
        DQSMT_B12 OFFSET(12) NUMBITS(1) [],
        DQSMT_B10 OFFSET(10) NUMBITS(1) [],
        DQSMT_LO OFFSET(0) NUMBITS(4) []
    ]
];

register_bitfields![u8,
    /// Misc BAR 0x0b00 (MCHBAR): processor-side init bit.
    pub MISC_B00_REG [
        PROC_SIDE_INIT OFFSET(0) NUMBITS(1) []
    ],

    /// Host Clock Timing Control (MCHBAR CxHCTC, 8-bit).
    pub HCTC_REG [
        HCTC_MODE OFFSET(0) NUMBITS(5) []
    ],

    /// Self-refresh status clear (MCHBAR SLFRCS, 8-bit).
    pub SLFRCS_REG [
        SR_CH1 OFFSET(1) NUMBITS(1) [],
        SR_CH0 OFFSET(0) NUMBITS(1) []
    ]
];

/// DCC command mask: bits the controller requires cleared around COMMAND.
pub const DCC_CMD_MASK: u32 = (3 << 21) | (1 << 20) | (1 << 19) | (7 << 16);
/// WCC reset-state base preserved by pre-JEDEC init.
pub const WCC_BASE_MASK: u32 = 0x113f_f3ff;
/// SBOCC reset-state base preserved by post-JEDEC init.
pub const SBOCC_BASE_MASK: u32 = 0xffbd_b6ff;
