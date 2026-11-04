//! DDRIO (DDR3 PHY) registers, IOSF port 0x0c (coreboot `soc/ddrio_regs.h`).
//!
//! DQ registers are per channel (0x2000 apart) and per module (0x800 apart,
//! two byte lanes each); CMD and CLKCTL are per channel (0x800 apart); the
//! compensation pad targets are per channel (0x100 apart); the rest is
//! global. Generated from the coreboot header, so field names follow it.
#![allow(missing_docs)]

use crate::baytrail::iosf::{DDRIO, IosfReg};
use tock_registers::register_bitfields;

pub const DQ_MOD_STRIDE: u32 = 0x0800;
pub const DQ_NUM_MODULES: u32 = 4;
pub const DQ_CHANNEL_STRIDE: u32 = 0x2000;
pub const DQ_NUM_CHANNELS: u32 = 2;
pub const CMD_CHANNEL_STRIDE: u32 = 0x0800;
pub const CLKCTL_CHANNEL_STRIDE: u32 = 0x0800;
pub const COMP_CHANNEL_STRIDE: u32 = 0x0100;
pub const COMP_NUM_CHANNELS: u32 = 2;

register_bitfields![u32,
    pub DQ_DLLTXCTL [
        GRP_OUT_EN_B1 OFFSET(0) NUMBITS(4) [],
        GRP_OUT_EN_B0 OFFSET(4) NUMBITS(4) [],
        GRP_PI_EN_B1 OFFSET(8) NUMBITS(4) [],
        GRP_PI_EN_B0 OFFSET(12) NUMBITS(4) []
    ],
    pub DQ_DLLRXCTL [
        RXDQS_OUT_EN_B1 OFFSET(0) NUMBITS(1) [],
        RXDQS_OUT_EN_B0 OFFSET(1) NUMBITS(1) [],
        RXDQ_OUT_EN_B1 OFFSET(2) NUMBITS(1) [],
        RXDQ_OUT_EN_B0 OFFSET(3) NUMBITS(1) []
    ],
    pub DQ_MDLLCTL [
        VREF_TUNING OFFSET(0) NUMBITS(4) [],
        VREG_CUR_SEL OFFSET(4) NUMBITS(3) [],
        VREG_EN OFFSET(13) NUMBITS(1) [],
        VREG_BYPASS OFFSET(14) NUMBITS(1) [],
        MDLL_EN OFFSET(17) NUMBITS(1) []
    ],
    pub DQ_B0RXIOBUFCTL [
        PWRDN OFFSET(7) NUMBITS(1) []
    ],
    pub DQ_B0VREFCTL [
        VREF_DECODE_B0 OFFSET(8) NUMBITS(7) []
    ],
    pub DQ_B1VREFCTL [
        VREF_DECODE_B1 OFFSET(8) NUMBITS(7) []
    ],
    pub DQ_DFTCTL [
        PSHRT_BIAS_B1 OFFSET(12) NUMBITS(1) [],
        PSHRT_BIAS_B0 OFFSET(13) NUMBITS(1) []
    ],
    pub DQ_DQTRAINSTS [
        SAMPLE_RCVEN_B1 OFFSET(0) NUMBITS(1) [],
        SAMPLE_RCVEN_B0 OFFSET(1) NUMBITS(1) [],
        SAMPLE_DQS_B1 OFFSET(8) NUMBITS(1) [],
        SAMPLE_DQS_B0 OFFSET(9) NUMBITS(1) []
    ],
    pub DQ_DLLPICODER0B1 [
        PICODE_GRP0_B1 OFFSET(0) NUMBITS(6) [],
        PICODE_DQ_B1 OFFSET(8) NUMBITS(6) [],
        PICODE_DQS_B1 OFFSET(16) NUMBITS(6) [],
        PICODE_RCVEN_B1 OFFSET(24) NUMBITS(6) []
    ],
    pub DQ_DLLPICODER0B0 [
        PICODE_GRP0_B0 OFFSET(0) NUMBITS(6) [],
        PICODE_DQ_B0 OFFSET(8) NUMBITS(6) [],
        PICODE_DQS_B0 OFFSET(16) NUMBITS(6) [],
        PICODE_RCVEN_B0 OFFSET(24) NUMBITS(6) []
    ],
    pub DQ_RXDQSPICODEB1 [
        PICODE_RXDQS_B1 OFFSET(0) NUMBITS(6) []
    ],
    pub DQ_RXDQSPICODEB0 [
        PICODE_RXDQS_B0 OFFSET(0) NUMBITS(6) []
    ],
    pub DQ_PTRCTL0 [
        RDPTR_OFFSET_DQ_B0 OFFSET(0) NUMBITS(4) [],
        RDPTR_OFFSET_DQS_B0 OFFSET(4) NUMBITS(4) [],
        RDPTR_OFFSET_RCVEN_B0 OFFSET(8) NUMBITS(4) [],
        RDPTR_OFFSET_DQ_B1 OFFSET(12) NUMBITS(4) [],
        RDPTR_OFFSET_DQS_B1 OFFSET(16) NUMBITS(4) [],
        RDPTR_OFFSET_RCVEN_B1 OFFSET(20) NUMBITS(4) []
    ],
    pub DQ_PTRCTL1 [
        WRPTR_INIT OFFSET(0) NUMBITS(3) [],
        RDPTR_INIT OFFSET(4) NUMBITS(4) [],
        DQ_FIFO_RST_N OFFSET(8) NUMBITS(1) []
    ],
    pub DQ_DBCTL0 [
        RK2_DBSEL_DQ_B0 OFFSET(0) NUMBITS(1) [],
        RK2_DBSEL_DQS_B0 OFFSET(1) NUMBITS(1) [],
        RK2_DBSEL_RCVEN_B0 OFFSET(2) NUMBITS(1) [],
        RK2_DBSEL_DQ_B1 OFFSET(3) NUMBITS(1) [],
        RK2_DBSEL_DQS_B1 OFFSET(4) NUMBITS(1) [],
        RK2_DBSEL_RCVEN_B1 OFFSET(5) NUMBITS(1) [],
        RK2_PLUS1SEL_DQ_B0 OFFSET(6) NUMBITS(1) [],
        RK2_PLUS1SEL_DQS_B0 OFFSET(7) NUMBITS(1) [],
        RK2_PLUS1SEL_RCVEN_B0 OFFSET(8) NUMBITS(1) [],
        RK2_PLUS1SEL_DQ_B1 OFFSET(9) NUMBITS(1) [],
        RK2_PLUS1SEL_DQS_B1 OFFSET(10) NUMBITS(1) [],
        RK2_PLUS1SEL_RCVEN_B1 OFFSET(11) NUMBITS(1) [],
        RK3_DBSEL_DQ_B0 OFFSET(16) NUMBITS(1) [],
        RK3_DBSEL_DQS_B0 OFFSET(17) NUMBITS(1) [],
        RK3_DBSEL_RCVEN_B0 OFFSET(18) NUMBITS(1) [],
        RK3_DBSEL_DQ_B1 OFFSET(19) NUMBITS(1) [],
        RK3_DBSEL_DQS_B1 OFFSET(20) NUMBITS(1) [],
        RK3_DBSEL_RCVEN_B1 OFFSET(21) NUMBITS(1) [],
        RK3_PLUS1SEL_DQ_B0 OFFSET(22) NUMBITS(1) [],
        RK3_PLUS1SEL_DQS_B0 OFFSET(23) NUMBITS(1) [],
        RK3_PLUS1SEL_RCVEN_B0 OFFSET(24) NUMBITS(1) [],
        RK3_PLUS1SEL_DQ_B1 OFFSET(25) NUMBITS(1) [],
        RK3_PLUS1SEL_DQS_B1 OFFSET(26) NUMBITS(1) [],
        RK3_PLUS1SEL_RCVEN_B1 OFFSET(27) NUMBITS(1) []
    ],
    pub DQ_DBCTL1 [
        RK0_DBSEL_DQ_B0 OFFSET(0) NUMBITS(1) [],
        RK0_DBSEL_DQS_B0 OFFSET(1) NUMBITS(1) [],
        RK0_DBSEL_RCVEN_B0 OFFSET(2) NUMBITS(1) [],
        RK0_DBSEL_DQ_B1 OFFSET(3) NUMBITS(1) [],
        RK0_DBSEL_DQS_B1 OFFSET(4) NUMBITS(1) [],
        RK0_DBSEL_RCVEN_B1 OFFSET(5) NUMBITS(1) [],
        RK0_PLUS1SEL_DQ_B0 OFFSET(6) NUMBITS(1) [],
        RK0_PLUS1SEL_DQS_B0 OFFSET(7) NUMBITS(1) [],
        RK0_PLUS1SEL_RCVEN_B0 OFFSET(8) NUMBITS(1) [],
        RK0_PLUS1SEL_DQ_B1 OFFSET(9) NUMBITS(1) [],
        RK0_PLUS1SEL_DQS_B1 OFFSET(10) NUMBITS(1) [],
        RK0_PLUS1SEL_RCVEN_B1 OFFSET(11) NUMBITS(1) [],
        RK1_DBSEL_DQ_B0 OFFSET(16) NUMBITS(1) [],
        RK1_DBSEL_DQS_B0 OFFSET(17) NUMBITS(1) [],
        RK1_DBSEL_RCVEN_B0 OFFSET(18) NUMBITS(1) [],
        RK1_DBSEL_DQ_B1 OFFSET(19) NUMBITS(1) [],
        RK1_DBSEL_DQS_B1 OFFSET(20) NUMBITS(1) [],
        RK1_DBSEL_RCVEN_B1 OFFSET(21) NUMBITS(1) [],
        RK1_PLUS1SEL_DQ_B0 OFFSET(22) NUMBITS(1) [],
        RK1_PLUS1SEL_DQS_B0 OFFSET(23) NUMBITS(1) [],
        RK1_PLUS1SEL_RCVEN_B0 OFFSET(24) NUMBITS(1) [],
        RK1_PLUS1SEL_DQ_B1 OFFSET(25) NUMBITS(1) [],
        RK1_PLUS1SEL_DQS_B1 OFFSET(26) NUMBITS(1) [],
        RK1_PLUS1SEL_RCVEN_B1 OFFSET(27) NUMBITS(1) []
    ],
    pub DQ_LATCTL0_B0 [
        WRCMD2DQSSTART_LATENCY_B0 OFFSET(0) NUMBITS(5) [],
        RDCMD2RCVEN_LATENCY_B0 OFFSET(8) NUMBITS(5) [],
        RDCMD2DATAVALID_LATENCY_B0 OFFSET(16) NUMBITS(6) []
    ],
    pub DQ_LATCTL0_B1 [
        WRCMD2DQSSTART_LATENCY_B1 OFFSET(0) NUMBITS(5) [],
        RDCMD2RCVEN_LATENCY_B1 OFFSET(8) NUMBITS(5) [],
        RDCMD2DATAVALID_LATENCY_B1 OFFSET(16) NUMBITS(6) []
    ],
    pub DQ_LATCTL1 [
        DIFFAMPEN_DELAY_B0 OFFSET(0) NUMBITS(5) [],
        MCHODT_DELAY_B0 OFFSET(8) NUMBITS(5) [],
        DIFFAMPEN_DELAY_B1 OFFSET(16) NUMBITS(5) [],
        MCHODT_DELAY_B1 OFFSET(24) NUMBITS(5) []
    ],
    pub DQ_ONDURCTL_B0 [
        RCVEN_LENGTH_B0 OFFSET(0) NUMBITS(6) [],
        DIFFAMPEN_LENGTH_B0 OFFSET(8) NUMBITS(6) [],
        MCHODT_LENGTH_B0 OFFSET(16) NUMBITS(6) []
    ],
    pub DQ_ONDURCTL_B1 [
        RCVEN_LENGTH_B1 OFFSET(0) NUMBITS(6) [],
        DIFFAMPEN_LENGTH_B1 OFFSET(8) NUMBITS(6) [],
        MCHODT_LENGTH_B1 OFFSET(16) NUMBITS(6) []
    ],
    pub DQ_OVRCTL_B0 [
        RXDLL_EN_OVR_VAL OFFSET(0) NUMBITS(1) [],
        RXDLL_EN_OVR OFFSET(1) NUMBITS(1) [],
        RXPI_EN_OVR_VAL OFFSET(2) NUMBITS(1) [],
        RXPI_EN_OVR OFFSET(3) NUMBITS(1) [],
        DQS_MCHODT_OVR_VAL_B0 OFFSET(10) NUMBITS(1) [],
        DQS_MCHODT_OVR_B0 OFFSET(11) NUMBITS(1) [],
        DM_MCHODT_OVR_VAL_B0 OFFSET(12) NUMBITS(1) [],
        DM_MCHODT_OVR_B0 OFFSET(13) NUMBITS(1) [],
        DQ_MCHODT_OVR_VAL_B0 OFFSET(14) NUMBITS(1) [],
        DQ_MCHODT_OVR_B0 OFFSET(15) NUMBITS(1) [],
        DQS_DIFFAMPEN_OVR_VAL_B0 OFFSET(16) NUMBITS(1) [],
        DQS_DIFFAMPEN_OVR_B0 OFFSET(17) NUMBITS(1) [],
        DM_DIFFAMPEN_OVR_VAL_B0 OFFSET(18) NUMBITS(1) [],
        DM_DIFFAMPEN_OVR_B0 OFFSET(19) NUMBITS(1) [],
        DQ_DIFFAMPEN_OVR_VAL_B0 OFFSET(20) NUMBITS(1) [],
        DQ_DIFFAMPEN_OVR_B0 OFFSET(21) NUMBITS(1) []
    ],
    pub DQ_OVRCTL_B1 [
        DQS_MCHODT_OVR_VAL_B1 OFFSET(10) NUMBITS(1) [],
        DQS_MCHODT_OVR_B1 OFFSET(11) NUMBITS(1) [],
        DM_MCHODT_OVR_VAL_B1 OFFSET(12) NUMBITS(1) [],
        DM_MCHODT_OVR_B1 OFFSET(13) NUMBITS(1) [],
        DQ_MCHODT_OVR_VAL_B1 OFFSET(14) NUMBITS(1) [],
        DQ_MCHODT_OVR_B1 OFFSET(15) NUMBITS(1) [],
        DQS_DIFFAMPEN_OVR_VAL_B1 OFFSET(16) NUMBITS(1) [],
        DQS_DIFFAMPEN_OVR_B1 OFFSET(17) NUMBITS(1) [],
        DM_DIFFAMPEN_OVR_VAL_B1 OFFSET(18) NUMBITS(1) [],
        DM_DIFFAMPEN_OVR_B1 OFFSET(19) NUMBITS(1) [],
        DQ_DIFFAMPEN_OVR_VAL_B1 OFFSET(20) NUMBITS(1) [],
        DQ_DIFFAMPEN_OVR_B1 OFFSET(21) NUMBITS(1) []
    ],
    pub DQ_DQCTL [
        BL_MODE OFFSET(20) NUMBITS(2) [],
        EARLY_DATAVALID_SEL_B0 OFFSET(29) NUMBITS(1) [],
        EARLY_DATAVALID_SEL_B1 OFFSET(30) NUMBITS(1) []
    ],
    pub DQ_RK2RK_PTRCTL [
        RK0_MIN_DQ_B1 OFFSET(0) NUMBITS(1) [],
        RK0_MIN_DQS_B1 OFFSET(1) NUMBITS(1) [],
        RK0_MIN_RCVEN_B1 OFFSET(2) NUMBITS(1) [],
        RK1_MIN_DQ_B1 OFFSET(3) NUMBITS(1) [],
        RK1_MIN_DQS_B1 OFFSET(4) NUMBITS(1) [],
        RK1_MIN_RCVEN_B1 OFFSET(5) NUMBITS(1) [],
        RK2_MIN_DQ_B1 OFFSET(6) NUMBITS(1) [],
        RK2_MIN_DQS_B1 OFFSET(7) NUMBITS(1) [],
        RK2_MIN_RCVEN_B1 OFFSET(8) NUMBITS(1) [],
        RK3_MIN_DQ_B1 OFFSET(9) NUMBITS(1) [],
        RK3_MIN_DQS_B1 OFFSET(10) NUMBITS(1) [],
        RK3_MIN_RCVEN_B1 OFFSET(11) NUMBITS(1) [],
        RK0_MIN_DQ_B0 OFFSET(16) NUMBITS(1) [],
        RK0_MIN_DQS_B0 OFFSET(17) NUMBITS(1) [],
        RK0_MIN_RCVEN_B0 OFFSET(18) NUMBITS(1) [],
        RK1_MIN_DQ_B0 OFFSET(19) NUMBITS(1) [],
        RK1_MIN_DQS_B0 OFFSET(20) NUMBITS(1) [],
        RK1_MIN_RCVEN_B0 OFFSET(21) NUMBITS(1) [],
        RK2_MIN_DQ_B0 OFFSET(22) NUMBITS(1) [],
        RK2_MIN_DQS_B0 OFFSET(23) NUMBITS(1) [],
        RK2_MIN_RCVEN_B0 OFFSET(24) NUMBITS(1) [],
        RK3_MIN_DQ_B0 OFFSET(25) NUMBITS(1) [],
        RK3_MIN_DQS_B0 OFFSET(26) NUMBITS(1) [],
        RK3_MIN_RCVEN_B0 OFFSET(27) NUMBITS(1) []
    ],
    pub DQ_CLKALIGN_REG2 [
        CKALIGN_MODID OFFSET(0) NUMBITS(4) []
    ],
    pub DQ_CLOCK_GATE [
        RX_CG_EN OFFSET(0) NUMBITS(1) [],
        RXFIFO_CG_CNT OFFSET(1) NUMBITS(7) [],
        TX_CG_EN OFFSET(8) NUMBITS(1) [],
        TXFIFO_CG_CNT OFFSET(9) NUMBITS(7) [],
        RAWCLK_CG_EN OFFSET(16) NUMBITS(1) []
    ],
    pub DQ_TIMING_CTRL [
        SEL_FAST_PATH_B0 OFFSET(0) NUMBITS(9) [],
        SEL_FAST_PATH_B1 OFFSET(16) NUMBITS(9) [],
        LPDDR2_XDDR3_B OFFSET(25) NUMBITS(1) [],
        CA_TRAINING_MODE OFFSET(28) NUMBITS(1) [],
        DQS_OFFSET_EN OFFSET(29) NUMBITS(1) []
    ],
    pub DQ_DQMOD_CTL [
        BYTELANE0_DIS OFFSET(0) NUMBITS(1) [],
        BYTELANE1_DIS OFFSET(1) NUMBITS(1) []
    ],
    pub DQ_IOBONUSB0 [
        BONUS_B0 OFFSET(0) NUMBITS(22) []
    ],
    pub DQ_IOBONUSB1 [
        BONUS_B1 OFFSET(0) NUMBITS(22) []
    ],
    pub DQ_IOCTL1 [
        DDRMODE_B0 OFFSET(0) NUMBITS(2) [],
        RXEN_B0 OFFSET(2) NUMBITS(2) [],
        DDRMODE_B1 OFFSET(16) NUMBITS(2) [],
        RXEN_B1 OFFSET(18) NUMBITS(2) []
    ],
    pub DQ_IOCTL2 [
        DLYCELL_STATIC_LEGENB_B1 OFFSET(0) NUMBITS(1) [],
        DLYCELL_STATIC_LEGENB_B0 OFFSET(1) NUMBITS(1) []
    ],
    pub DQ_DLLCTL1 [
        NODLL_EN OFFSET(12) NUMBITS(1) [],
        TX_DIG_DLL_DLYCELL_EN OFFSET(24) NUMBITS(3) []
    ],
    pub DQ_CLOCK_GATE2 [
        TXDQDQS_DLL_DYNPI_EN_B0 OFFSET(0) NUMBITS(1) [],
        TXDQDQS_DLL_PIDIS_MAXCNT_B0 OFFSET(1) NUMBITS(6) [],
        TXDLL_DYNPI_EN_STG_B0 OFFSET(7) NUMBITS(1) [],
        TXRCVEN_DLL_DYNPI_EN_B0 OFFSET(8) NUMBITS(1) [],
        TXRCVEN_DLL_PIDIS_MAXCNT_B0 OFFSET(9) NUMBITS(7) [],
        TXDQDQS_DLL_DYNPI_EN_B1 OFFSET(16) NUMBITS(1) [],
        TXDQDQS_DLL_PIDIS_MAXCNT_B1 OFFSET(17) NUMBITS(6) [],
        TXDLL_DYNPI_EN_STG_B1 OFFSET(23) NUMBITS(1) [],
        TXRCVEN_DLL_DYNPI_EN_B1 OFFSET(24) NUMBITS(1) [],
        TXRCVEN_DLL_PIDIS_MAXCNT_B1 OFFSET(25) NUMBITS(7) []
    ],
    pub DQ_CLOCK_GATE3 [
        RX_PI_DRV_STOP_GB OFFSET(0) NUMBITS(3) [],
        RX_PI_DRV_START_GB OFFSET(4) NUMBITS(3) [],
        TX_PI_DRV_STOP_GB OFFSET(8) NUMBITS(3) [],
        TX_PI_DRV_START_GB OFFSET(12) NUMBITS(3) []
    ],
    pub DQ_IOCTL3 [
        DQS_PU_OFFSET_B0 OFFSET(0) NUMBITS(2) [],
        DQS_PD_OFFSET_B0 OFFSET(2) NUMBITS(2) [],
        DQS_PU_OFFSET_B1 OFFSET(4) NUMBITS(2) [],
        DQS_PD_OFFSET_B1 OFFSET(6) NUMBITS(2) [],
        RX_DDLL_POWERGATE_DIS OFFSET(8) NUMBITS(1) []
    ],
    pub CMD_PGSTAGGER [
        DIG2ANA_PGEN_STG_DELAY OFFSET(0) NUMBITS(8) [],
        ANA2DIG_PGDIS_STG_DELAY OFFSET(8) NUMBITS(8) [],
        CLKGATE_WAIT_TIMER OFFSET(16) NUMBITS(4) [],
        PGSTG_MOD_GRP OFFSET(20) NUMBITS(2) [],
        IOPWRGATE_DIS OFFSET(22) NUMBITS(1) []
    ],
    pub CMD_DLLTXCTL [
        GRP_OUT_EN_B1 OFFSET(0) NUMBITS(4) [],
        GRP_OUT_EN_B0 OFFSET(4) NUMBITS(4) [],
        GRP_PI_EN_B1 OFFSET(8) NUMBITS(4) [],
        GRP_PI_EN_B0 OFFSET(12) NUMBITS(4) []
    ],
    pub CMD_MDLLCTL [
        VREF_TUNING OFFSET(0) NUMBITS(4) [],
        VREG_CUR_SEL OFFSET(4) NUMBITS(3) [],
        VREG_EN_H OFFSET(13) NUMBITS(1) [],
        VREG_BYPASS OFFSET(14) NUMBITS(1) [],
        MDLL_EN OFFSET(18) NUMBITS(1) []
    ],
    pub CMD_DLLPICODER0 [
        PICODE_WCTL_CMD OFFSET(0) NUMBITS(6) [],
        PICODE_GRP2_B1 OFFSET(8) NUMBITS(6) [],
        PICODE_GRP3_B0 OFFSET(16) NUMBITS(6) [],
        PICODE_GRP3_B1 OFFSET(24) NUMBITS(6) []
    ],
    pub CMD_DLLPICODER1 [
        PICODE_CMD OFFSET(16) NUMBITS(6) [],
        PICODE_GRP1_B1 OFFSET(24) NUMBITS(6) []
    ],
    pub CMD_CFG_REG0 [
        TRI_STATE_OVR_EN OFFSET(0) NUMBITS(1) [],
        TRI_STATE_OVR_VAL OFFSET(1) NUMBITS(1) [],
        IOBUFACT_RST_N OFFSET(2) NUMBITS(1) [],
        SENSE_AMP_CONTROL OFFSET(3) NUMBITS(1) [],
        CKE_BUF_CONTROL OFFSET(8) NUMBITS(4) [],
        CKECMD_SENSE_AMP_CONTROL OFFSET(12) NUMBITS(4) [],
        DEADBAND_SEL OFFSET(16) NUMBITS(1) [],
        PTRPLUS1_SEL OFFSET(17) NUMBITS(1) [],
        CKECMD_DEADBAND_SEL0 OFFSET(18) NUMBITS(1) [],
        CKECMD_DEADBAND_SEL1 OFFSET(19) NUMBITS(1) [],
        CKECMD_DEADBAND_SEL2 OFFSET(20) NUMBITS(1) [],
        CKECMD_DEADBAND_SEL3 OFFSET(21) NUMBITS(1) [],
        CKECMD_PTRPLUS1_SEL0 OFFSET(24) NUMBITS(1) [],
        CKECMD_PTRPLUS1_SEL1 OFFSET(25) NUMBITS(1) [],
        CKECMD_PTRPLUS1_SEL2 OFFSET(26) NUMBITS(1) [],
        CKECMD_PTRPLUS1_SEL3 OFFSET(27) NUMBITS(1) []
    ],
    pub CMD_POINTER_REG [
        WRPTR_EN OFFSET(0) NUMBITS(1) [],
        WRPTR_INIT OFFSET(1) NUMBITS(3) [],
        RDPTR_INIT OFFSET(4) NUMBITS(4) [],
        RDPTR_OFFSET0 OFFSET(8) NUMBITS(4) [],
        CKECMD_RDPTR_OFFSET0 OFFSET(12) NUMBITS(4) [],
        CKECMD_RDPTR_OFFSET1 OFFSET(16) NUMBITS(4) [],
        CKECMD_RDPTR_OFFSET2 OFFSET(20) NUMBITS(4) [],
        CKECMD_RDPTR_OFFSET3 OFFSET(24) NUMBITS(4) [],
        RDPTR_OFFSET5 OFFSET(28) NUMBITS(4) []
    ],
    pub CMD_CLKALIGN_REG0 [
        CKALIGN_MODE OFFSET(4) NUMBITS(2) [],
        START_CLKALIGN OFFSET(24) NUMBITS(1) [],
        PG_CLKALIGN_EN OFFSET(25) NUMBITS(1) []
    ],
    pub CMD_CLKALIGN_REG1 [
        MICRO_PISTEP OFFSET(0) NUMBITS(2) [],
        MACRO_PISTEP OFFSET(2) NUMBITS(6) [],
        MAXSAMPLE_CNT OFFSET(8) NUMBITS(7) [],
        SAMPLE_SIZE OFFSET(16) NUMBITS(7) []
    ],
    pub CMD_CLKALIGN_REG2 [
        CKALIGN_MODID OFFSET(0) NUMBITS(4) [],
        CKALIGN_SDIRMOD OFFSET(4) NUMBITS(4) [],
        CKALIGN_NUMMOD OFFSET(8) NUMBITS(4) [],
        DLL_WT_CNT OFFSET(16) NUMBITS(5) []
    ],
    pub CMD_PM_CONFIG0 [
        PMMSG_NUM_MODULES OFFSET(0) NUMBITS(4) [],
        PMMSG_CLKSEL OFFSET(4) NUMBITS(1) [],
        PMMSG_CLK_SYNC_MODE OFFSET(5) NUMBITS(1) [],
        PMBLOCK_DIFFAMP_EN_OVR OFFSET(6) NUMBITS(1) [],
        PMMSG_WAITTIME OFFSET(8) NUMBITS(4) [],
        PMBLOCK_SET_PM_NACK OFFSET(12) NUMBITS(1) [],
        PMBLOCK_SET_PM_ACK OFFSET(13) NUMBITS(1) [],
        PM_MSG OFFSET(14) NUMBITS(5) [],
        PM_REQ OFFSET(19) NUMBITS(1) [],
        SPID_INIT_COMPLETE OFFSET(20) NUMBITS(1) [],
        PMBLOCK_MPLL_DIVRST_OVR OFFSET(21) NUMBITS(1) [],
        PMBLOCK_PTR_EN_OVR OFFSET(22) NUMBITS(1) [],
        PMBLOCK_CLKALIGN_OVR OFFSET(23) NUMBITS(1) [],
        PMBLOCK_SFR_EN_OVR OFFSET(24) NUMBITS(1) [],
        PMBLOCK_MPLL_EN_OVR OFFSET(25) NUMBITS(1) [],
        PMBLOCK_MPLL_GATE_OVR OFFSET(26) NUMBITS(1) [],
        PMBLOCK_MDLL_VREGON_OVR OFFSET(27) NUMBITS(1) [],
        PMBLOCK_MDLL_EN_OVR OFFSET(28) NUMBITS(1) [],
        PMBLOCK_REFDLL_EN_OVR OFFSET(29) NUMBITS(1) [],
        PMBLOCK_SIGDLL_EN_OVR OFFSET(30) NUMBITS(1) [],
        PMBLOCK_IOBUFACT_OVR OFFSET(31) NUMBITS(1) []
    ],
    pub CMD_PM_DLYREG0 [
        PMMSG_IOBUFACT_DELAY OFFSET(0) NUMBITS(8) [],
        PMMSG_DLLPI_EN_DELAY OFFSET(8) NUMBITS(8) [],
        PMMSG_MDLL_EN_STG_DELAY OFFSET(16) NUMBITS(8) [],
        PMMSG_MPLL_UNGATE_DELAY OFFSET(24) NUMBITS(8) []
    ],
    pub CMD_PM_DLYREG1 [
        PMMSG_DLLPI_DIS_DELAY OFFSET(0) NUMBITS(8) [],
        PMMSG_MDLL_DIS_DELAY OFFSET(8) NUMBITS(8) [],
        PMMSG_MDLL_DIS_STG_DELAY OFFSET(16) NUMBITS(8) [],
        PMMSG_MDLL_VREGOFF_DELAY OFFSET(24) NUMBITS(8) []
    ],
    pub CMD_PM_DLYREG2 [
        PMMSG_MPLL_GATE_DELAY OFFSET(0) NUMBITS(8) [],
        PMMSG_MPLL_DIS_DELAY OFFSET(8) NUMBITS(8) [],
        PMMSG_SFR_DIS_DELAY OFFSET(16) NUMBITS(8) [],
        PMMSG_MPLL_DIVRST_CNT OFFSET(24) NUMBITS(8) []
    ],
    pub CMD_PM_DLYREG3 [
        PMMSG_MDLL_EN_DELAY OFFSET(0) NUMBITS(12) [],
        PMMSG_ALLON_IOBUFACT_DELAY OFFSET(12) NUMBITS(4) [],
        PMMSG_MDLL_VREGON_DELAY OFFSET(16) NUMBITS(12) [],
        PMMSG_ASRT_IOBUFACT_ALLON0_DELAY OFFSET(28) NUMBITS(4) []
    ],
    pub CMD_PM_DLYREG4 [
        PMMSG_MPLL_EN_DELAY OFFSET(0) NUMBITS(16) [],
        PMMSG_SFR_EN_DELAY OFFSET(16) NUMBITS(16) []
    ],
    pub CMD_BONUSCMD0_IO [
        BONUS_CMD0 OFFSET(8) NUMBITS(24) []
    ],
    pub CMD_CLKGATE [
        RAWCLK_GATE_EN OFFSET(0) NUMBITS(1) [],
        M1XCLK_GATE_EN OFFSET(8) NUMBITS(1) [],
        CMDSLICE_EN_B OFFSET(9) NUMBITS(18) []
    ],
    pub CMD_TIMING_CTRL [
        SEL_FAST_PATH OFFSET(0) NUMBITS(1) [],
        CKECMD_SEL_FAST_PATH0 OFFSET(1) NUMBITS(1) [],
        CKECMD_SEL_FAST_PATH1 OFFSET(2) NUMBITS(1) [],
        CKECMD_SEL_FAST_PATH2 OFFSET(3) NUMBITS(1) [],
        CKECMD_SEL_FAST_PATH3 OFFSET(4) NUMBITS(1) [],
        LPDDR2_XDDR3_B OFFSET(5) NUMBITS(1) [],
        GLOBAL_PWRGATE_EN OFFSET(7) NUMBITS(1) [],
        ISOL2PG_DELAY OFFSET(8) NUMBITS(4) [],
        RST_WAIT_TIMER OFFSET(12) NUMBITS(4) [],
        MOD2MOD_PGEN_STG_DELAY OFFSET(16) NUMBITS(8) [],
        MOD2MOD_PGDIS_STG_DELAY OFFSET(24) NUMBITS(8) []
    ],
    pub CMD_IOCTLB0 [
        NODLL_EN OFFSET(6) NUMBITS(1) []
    ],
    pub CMD_DLLCTL1 [
        TX_DIG_DLL_DLYCELL_EN OFFSET(0) NUMBITS(3) []
    ],
    pub CMD_PGOVR [
        COMP_TESTCLK_OVR OFFSET(0) NUMBITS(1) [],
        PGPHYRST_B_OVR OFFSET(1) NUMBITS(1) [],
        CLKGATE_EN_OVR OFFSET(2) NUMBITS(1) [],
        ISOLEN_OVR OFFSET(3) NUMBITS(1) [],
        IOISOLEN_OVR OFFSET(4) NUMBITS(1) [],
        PMBLOCK_DDLL_EN_OVR OFFSET(5) NUMBITS(1) [],
        CMD_DQ0_IOPWRGATE_EN_OVR OFFSET(6) NUMBITS(1) [],
        CMD_DQ1_IOPWRGATE_EN_OVR OFFSET(7) NUMBITS(1) [],
        CMD_DQ2_IOPWRGATE_EN_OVR OFFSET(8) NUMBITS(1) [],
        CMD_DQ3_IOPWRGATE_EN_OVR OFFSET(9) NUMBITS(1) [],
        CMD_CMD_IOPWRGATE_EN_OVR OFFSET(10) NUMBITS(1) [],
        CMD_CLKCTL_IOPWRGATE_EN_OVR OFFSET(11) NUMBITS(1) [],
        CMD_PLLCOMP_IOPWRGATE_EN_OVR OFFSET(12) NUMBITS(1) [],
        CMD_DQ0_PWRGATE_EN_OVR OFFSET(13) NUMBITS(1) [],
        CMD_DQ1_PWRGATE_EN_OVR OFFSET(14) NUMBITS(1) [],
        CMD_DQ2_PWRGATE_EN_OVR OFFSET(15) NUMBITS(1) [],
        CMD_DQ3_PWRGATE_EN_OVR OFFSET(16) NUMBITS(1) [],
        CMD_CMD_PWRGATE_EN_OVR OFFSET(17) NUMBITS(1) [],
        CMD_CLKCTL_PWRGATE_EN_OVR OFFSET(18) NUMBITS(1) [],
        CMD_PLLCOMP_PWRGATE_EN_OVR OFFSET(19) NUMBITS(1) []
    ],
    pub CLKCTL_DLLTXCTL [
        GRP_OUT_EN_B1 OFFSET(0) NUMBITS(4) [],
        GRP_OUT_EN_B0 OFFSET(4) NUMBITS(4) [],
        GRP_PI_EN_B1 OFFSET(8) NUMBITS(4) [],
        GRP_PI_EN_B0 OFFSET(12) NUMBITS(4) []
    ],
    pub CLKCTL_MDLLCTL [
        VREF_TUNING OFFSET(0) NUMBITS(4) [],
        VREG_CUR_SEL OFFSET(4) NUMBITS(3) [],
        VREG_EN_H OFFSET(13) NUMBITS(1) [],
        VREG_BYPASS OFFSET(14) NUMBITS(1) [],
        MDLL_EN OFFSET(18) NUMBITS(1) [],
        NODLL_EN OFFSET(23) NUMBITS(1) []
    ],
    pub CLKCTL_DLLPICODER0 [
        PICODE_GRP2_B0 OFFSET(0) NUMBITS(6) [],
        PICODE_WCTL OFFSET(8) NUMBITS(6) [],
        PICODE_GRP3_B0 OFFSET(16) NUMBITS(6) [],
        PICODE_GRP3_B1 OFFSET(24) NUMBITS(6) []
    ],
    pub CLKCTL_DLLPICODER1 [
        PICODE_CMD_CLKCTL OFFSET(8) NUMBITS(6) [],
        PICODE_WCLK OFFSET(16) NUMBITS(6) [],
        PICODE_GRP1_B1 OFFSET(24) NUMBITS(6) []
    ],
    pub CLKCTL_DDR3RESETCTL [
        DDR3_RESET_RESET OFFSET(1) NUMBITS(1) [],
        DDR3_RESET_SET OFFSET(8) NUMBITS(1) [],
        WR_LEVEL OFFSET(16) NUMBITS(1) []
    ],
    pub CLKCTL_CFG_REG0 [
        POS1NEG0_SEL OFFSET(0) NUMBITS(1) [],
        CLK_BUF_CONTROL OFFSET(4) NUMBITS(4) [],
        ODT_BUF_CONTROL OFFSET(8) NUMBITS(4) [],
        CKE_BUF_CONTROL OFFSET(12) NUMBITS(4) [],
        CS_BUF_CONTROL OFFSET(16) NUMBITS(4) [],
        CKLOW_XCKTRI_B OFFSET(20) NUMBITS(1) []
    ],
    pub CLKCTL_CFG_REG1 [
        DEADBAND_SEL OFFSET(0) NUMBITS(6) [],
        PTRPLUS1_SEL OFFSET(8) NUMBITS(6) []
    ],
    pub CLKCTL_POINTER_REG [
        WRPTR_EN OFFSET(0) NUMBITS(1) [],
        WRPTR_INIT OFFSET(1) NUMBITS(3) [],
        RDPTR_INIT OFFSET(4) NUMBITS(4) [],
        RDPTR_OFFSET0 OFFSET(8) NUMBITS(4) [],
        RDPTR_OFFSET1 OFFSET(12) NUMBITS(4) [],
        RDPTR_OFFSET2 OFFSET(16) NUMBITS(4) [],
        RDPTR_OFFSET3 OFFSET(20) NUMBITS(4) [],
        RDPTR_OFFSET4 OFFSET(24) NUMBITS(4) [],
        RDPTR_OFFSET5 OFFSET(28) NUMBITS(4) []
    ],
    pub CLKCTL_CLKALIGN_REG2 [
        CKALIGN_MODID OFFSET(0) NUMBITS(4) []
    ],
    pub CLKCTL_BONUSCLK_IO [
        BONUS_CLK OFFSET(8) NUMBITS(24) []
    ],
    pub CLKCTL_CLKGATE [
        RAWCLK_GATE_EN OFFSET(0) NUMBITS(1) [],
        M1XCLK_GATE_EN OFFSET(8) NUMBITS(1) [],
        CMDSLICE_EN_B OFFSET(9) NUMBITS(4) []
    ],
    pub CLKCTL_TIMING_CTRL [
        SEL_FAST_PATH OFFSET(0) NUMBITS(2) [],
        LPDDR2_XDDR3_B OFFSET(2) NUMBITS(1) [],
        CLK_SEL_FAST_PATH OFFSET(4) NUMBITS(2) []
    ],
    pub CLKCTL_CMD_PTRCTL [
        CMD_PTRPLUS1_SEL OFFSET(0) NUMBITS(1) [],
        CMD_DEADBAND_SEL OFFSET(1) NUMBITS(1) [],
        RDPTR_OFFSET6 OFFSET(8) NUMBITS(4) []
    ],
    pub CLKCTL_CMD_CFG_REG2 [
        CMD_TRI_STATE_OVR_EN OFFSET(0) NUMBITS(1) [],
        CMD_TRI_STATE_OVR_VAL OFFSET(1) NUMBITS(1) []
    ],
    pub CLKCTL_DLLCTL1 [
        TX_DIG_DLL_DLYCELL_EN OFFSET(0) NUMBITS(3) []
    ],
    pub CMPCTRL [
        INIT_COMP OFFSET(0) NUMBITS(1) [],
        RCOMP_EN OFFSET(1) NUMBITS(1) [],
        CLKGATE_DIS OFFSET(4) NUMBITS(1) [],
        STEP_CNT OFFSET(12) NUMBITS(4) [],
        STL_DELAY OFFSET(16) NUMBITS(8) []
    ],
    pub NMSCNTRL [
        NMS_CNT OFFSET(0) NUMBITS(12) []
    ],
    pub LATCH1CTL [
        CHNL_CNT OFFSET(28) NUMBITS(3) []
    ],
    pub TCOCNTCTRL [
        TCO_CNT_VAL OFFSET(0) NUMBITS(2) []
    ],
    pub CHNLBUFSTATIC [
        DQ_STAT_ODT_SEG_EN OFFSET(0) NUMBITS(2) [],
        DQ_STAT_DRV_SEG_EN OFFSET(4) NUMBITS(4) [],
        CC_STAT_ODT_SEG_EN OFFSET(8) NUMBITS(2) [],
        CC_STAT_DRV_SEG_EN OFFSET(12) NUMBITS(4) []
    ],
    pub DQVREF [
        DQ_ODT_PD_INT OFFSET(0) NUMBITS(7) [],
        DQ_ODT_PU_EXT OFFSET(8) NUMBITS(7) [],
        DQ_DRV_PD_INT OFFSET(16) NUMBITS(7) [],
        VREF_SEL OFFSET(23) NUMBITS(1) [],
        DQ_DRV_PD_EXT OFFSET(24) NUMBITS(7) [],
        EXT_VREF_SEL OFFSET(31) NUMBITS(1) []
    ],
    pub CMDVREF [
        CMD_DRV_PD_INT OFFSET(16) NUMBITS(7) [],
        CMD_DRV_PD_EXT OFFSET(24) NUMBITS(7) []
    ],
    pub CLKVREF [
        CLK_ODT_PD_INT OFFSET(0) NUMBITS(7) [],
        CLK_ODT_PU_EXT OFFSET(8) NUMBITS(7) [],
        CLK_DRV_PD_INT OFFSET(16) NUMBITS(7) [],
        CLK_DRV_PD_EXT OFFSET(24) NUMBITS(7) []
    ],
    pub DQSVREF [
        DQS_ODT_PD_INT OFFSET(0) NUMBITS(7) [],
        DQS_ODT_PU_EXT OFFSET(8) NUMBITS(7) [],
        DQS_DRV_PD_INT OFFSET(16) NUMBITS(7) [],
        DQS_DRV_PD_EXT OFFSET(24) NUMBITS(7) []
    ],
    pub CTLVREF [
        CTL_DRV_PD_INT OFFSET(16) NUMBITS(7) [],
        CTL_DRV_PD_EXT OFFSET(24) NUMBITS(7) []
    ],
    pub TCOVREF [
        DQ_TCO_VREF OFFSET(0) NUMBITS(7) [],
        DQS_TCO_VREF OFFSET(8) NUMBITS(7) [],
        CLK_TCO_VREF OFFSET(16) NUMBITS(7) []
    ],
    pub DLYSEL [
        DQ_DCMP_SEL OFFSET(0) NUMBITS(4) [],
        DQS_DCMP_SEL OFFSET(4) NUMBITS(4) [],
        CLK_DCMP_SEL OFFSET(8) NUMBITS(4) [],
        CMD_DCMP_SEL OFFSET(12) NUMBITS(4) [],
        CTL_DCMP_SEL OFFSET(16) NUMBITS(4) [],
        RXDLL_DCMP_SEL OFFSET(20) NUMBITS(4) [],
        TXDLL_DCMP_SEL OFFSET(24) NUMBITS(4) []
    ],
    pub TCODRAMBUFODT [
        TCO_DBUF_PD_STATIC OFFSET(7) NUMBITS(1) [],
        TCO_DBUF_PU_STATIC OFFSET(15) NUMBITS(1) []
    ],
    pub COMPEN4 [
        CMDCTL_TCO_EN OFFSET(0) NUMBITS(1) []
    ],
    pub MPLLCTRL [
        DDR_FREQ OFFSET(4) NUMBITS(4) [],
        VCO_DIS OFFSET(24) NUMBITS(1) [],
        DIVRST_N OFFSET(26) NUMBITS(1) []
    ],
    pub MPLLCTRL1 [
        CLKGATE_EN OFFSET(0) NUMBITS(1) [],
        CHANNEL1_ECC_EN OFFSET(1) NUMBITS(1) []
    ],
    pub MPLLCSR0 [
        SFR_CFG OFFSET(16) NUMBITS(8) [],
        SFR_EN OFFSET(24) NUMBITS(1) []
    ],
    pub MPLLCSR1 [
        PLL_MODE OFFSET(3) NUMBITS(1) [],
        DTYCYC_DEL OFFSET(16) NUMBITS(8) []
    ],
    pub MPLLMISC [
        CH1_CLK_CKEBB_EN OFFSET(0) NUMBITS(1) [],
        CH0_CLK_CKEBB_EN OFFSET(1) NUMBITS(1) [],
        MPLL_SFR_SUPPLY_MODE OFFSET(3) NUMBITS(2) [],
        MPLL_2XCLK_SEL OFFSET(24) NUMBITS(2) []
    ],
    pub MPLLDFTOUT [
        PLL_CNT_DONE OFFSET(1) NUMBITS(1) [],
        SFR_CNT_DONE OFFSET(2) NUMBITS(1) [],
        DTYCYC_DONE OFFSET(3) NUMBITS(1) []
    ],
    pub MASTERRSTN [
        PHYRST_N OFFSET(0) NUMBITS(1) []
    ],
    pub PLLLOCKDEL [
        PLL_WRMUP_DEL OFFSET(0) NUMBITS(16) []
    ],
    pub SFRDEL [
        SFR_STBL_DEL OFFSET(0) NUMBITS(16) []
    ]
];

pub const fn dq_dlltxctl(ch: u32, module: u32) -> IosfReg<DQ_DLLTXCTL::Register> {
    DDRIO.reg(0x0004 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dllrxctl(ch: u32, module: u32) -> IosfReg<DQ_DLLRXCTL::Register> {
    DDRIO.reg(0x0008 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_mdllctl(ch: u32, module: u32) -> IosfReg<DQ_MDLLCTL::Register> {
    DDRIO.reg(0x000c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_b0rxiobufctl(ch: u32, module: u32) -> IosfReg<DQ_B0RXIOBUFCTL::Register> {
    DDRIO.reg(0x0010 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_b0vrefctl(ch: u32, module: u32) -> IosfReg<DQ_B0VREFCTL::Register> {
    DDRIO.reg(0x0014 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_b1rxiobufctl(ch: u32, module: u32) -> IosfReg<DQ_B0RXIOBUFCTL::Register> {
    DDRIO.reg(0x0020 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_b1vrefctl(ch: u32, module: u32) -> IosfReg<DQ_B1VREFCTL::Register> {
    DDRIO.reg(0x0024 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dftctl(ch: u32, module: u32) -> IosfReg<DQ_DFTCTL::Register> {
    DDRIO.reg(0x0030 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dqtrainsts(ch: u32, module: u32) -> IosfReg<DQ_DQTRAINSTS::Register> {
    DDRIO.reg(0x0034 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dllpicoder0b1(ch: u32, module: u32) -> IosfReg<DQ_DLLPICODER0B1::Register> {
    DDRIO.reg(0x0038 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dllpicoder0b0(ch: u32, module: u32) -> IosfReg<DQ_DLLPICODER0B0::Register> {
    DDRIO.reg(0x003c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_rxdqspicodeb1(ch: u32, module: u32) -> IosfReg<DQ_RXDQSPICODEB1::Register> {
    DDRIO.reg(0x0058 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_rxdqspicodeb0(ch: u32, module: u32) -> IosfReg<DQ_RXDQSPICODEB0::Register> {
    DDRIO.reg(0x005c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ptrctl0(ch: u32, module: u32) -> IosfReg<DQ_PTRCTL0::Register> {
    DDRIO.reg(0x0070 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ptrctl1(ch: u32, module: u32) -> IosfReg<DQ_PTRCTL1::Register> {
    DDRIO.reg(0x0074 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dbctl0(ch: u32, module: u32) -> IosfReg<DQ_DBCTL0::Register> {
    DDRIO.reg(0x0078 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dbctl1(ch: u32, module: u32) -> IosfReg<DQ_DBCTL1::Register> {
    DDRIO.reg(0x007c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_latctl0_b0(ch: u32, module: u32) -> IosfReg<DQ_LATCTL0_B0::Register> {
    DDRIO.reg(0x0080 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_latctl0_b1(ch: u32, module: u32) -> IosfReg<DQ_LATCTL0_B1::Register> {
    DDRIO.reg(0x0084 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_latctl1(ch: u32, module: u32) -> IosfReg<DQ_LATCTL1::Register> {
    DDRIO.reg(0x0088 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ondurctl_b0(ch: u32, module: u32) -> IosfReg<DQ_ONDURCTL_B0::Register> {
    DDRIO.reg(0x008c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ondurctl_b1(ch: u32, module: u32) -> IosfReg<DQ_ONDURCTL_B1::Register> {
    DDRIO.reg(0x0090 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ovrctl_b0(ch: u32, module: u32) -> IosfReg<DQ_OVRCTL_B0::Register> {
    DDRIO.reg(0x0094 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ovrctl_b1(ch: u32, module: u32) -> IosfReg<DQ_OVRCTL_B1::Register> {
    DDRIO.reg(0x0098 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dqctl(ch: u32, module: u32) -> IosfReg<DQ_DQCTL::Register> {
    DDRIO.reg(0x009c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_rk2rk_ptrctl(ch: u32, module: u32) -> IosfReg<DQ_RK2RK_PTRCTL::Register> {
    DDRIO.reg(0x00ac + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_clkalign_reg2(ch: u32, module: u32) -> IosfReg<DQ_CLKALIGN_REG2::Register> {
    DDRIO.reg(0x00c0 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_clock_gate(ch: u32, module: u32) -> IosfReg<DQ_CLOCK_GATE::Register> {
    DDRIO.reg(0x00cc + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_timing_ctrl(ch: u32, module: u32) -> IosfReg<DQ_TIMING_CTRL::Register> {
    DDRIO.reg(0x010c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dqmod_ctl(ch: u32, module: u32) -> IosfReg<DQ_DQMOD_CTL::Register> {
    DDRIO.reg(0x0110 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_iobonusb0(ch: u32, module: u32) -> IosfReg<DQ_IOBONUSB0::Register> {
    DDRIO.reg(0x0114 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_iobonusb1(ch: u32, module: u32) -> IosfReg<DQ_IOBONUSB1::Register> {
    DDRIO.reg(0x0118 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ioctl1(ch: u32, module: u32) -> IosfReg<DQ_IOCTL1::Register> {
    DDRIO.reg(0x011c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ioctl2(ch: u32, module: u32) -> IosfReg<DQ_IOCTL2::Register> {
    DDRIO.reg(0x0120 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_dllctl1(ch: u32, module: u32) -> IosfReg<DQ_DLLCTL1::Register> {
    DDRIO.reg(0x0134 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_clock_gate2(ch: u32, module: u32) -> IosfReg<DQ_CLOCK_GATE2::Register> {
    DDRIO.reg(0x0138 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_clock_gate3(ch: u32, module: u32) -> IosfReg<DQ_CLOCK_GATE3::Register> {
    DDRIO.reg(0x013c + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn dq_ioctl3(ch: u32, module: u32) -> IosfReg<DQ_IOCTL3::Register> {
    DDRIO.reg(0x0140 + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
}
pub const fn cmd_pgstagger(ch: u32) -> IosfReg<CMD_PGSTAGGER::Register> {
    DDRIO.reg(0x4804 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_dlltxctl(ch: u32) -> IosfReg<CMD_DLLTXCTL::Register> {
    DDRIO.reg(0x4808 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_mdllctl(ch: u32) -> IosfReg<CMD_MDLLCTL::Register> {
    DDRIO.reg(0x4810 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_dllpicoder0(ch: u32) -> IosfReg<CMD_DLLPICODER0::Register> {
    DDRIO.reg(0x4820 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_dllpicoder1(ch: u32) -> IosfReg<CMD_DLLPICODER1::Register> {
    DDRIO.reg(0x4824 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_cfg_reg0(ch: u32) -> IosfReg<CMD_CFG_REG0::Register> {
    DDRIO.reg(0x4840 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pointer_reg(ch: u32) -> IosfReg<CMD_POINTER_REG::Register> {
    DDRIO.reg(0x4844 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_clkalign_reg0(ch: u32) -> IosfReg<CMD_CLKALIGN_REG0::Register> {
    DDRIO.reg(0x4850 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_clkalign_reg1(ch: u32) -> IosfReg<CMD_CLKALIGN_REG1::Register> {
    DDRIO.reg(0x4854 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_clkalign_reg2(ch: u32) -> IosfReg<CMD_CLKALIGN_REG2::Register> {
    DDRIO.reg(0x4858 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_config0(ch: u32) -> IosfReg<CMD_PM_CONFIG0::Register> {
    DDRIO.reg(0x485c + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_dlyreg0(ch: u32) -> IosfReg<CMD_PM_DLYREG0::Register> {
    DDRIO.reg(0x4860 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_dlyreg1(ch: u32) -> IosfReg<CMD_PM_DLYREG1::Register> {
    DDRIO.reg(0x4864 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_dlyreg2(ch: u32) -> IosfReg<CMD_PM_DLYREG2::Register> {
    DDRIO.reg(0x4868 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_dlyreg3(ch: u32) -> IosfReg<CMD_PM_DLYREG3::Register> {
    DDRIO.reg(0x486c + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pm_dlyreg4(ch: u32) -> IosfReg<CMD_PM_DLYREG4::Register> {
    DDRIO.reg(0x4870 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_bonuscmd0_io(ch: u32) -> IosfReg<CMD_BONUSCMD0_IO::Register> {
    DDRIO.reg(0x488c + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_clkgate(ch: u32) -> IosfReg<CMD_CLKGATE::Register> {
    DDRIO.reg(0x48a0 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_timing_ctrl(ch: u32) -> IosfReg<CMD_TIMING_CTRL::Register> {
    DDRIO.reg(0x48a4 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_ioctlb0(ch: u32) -> IosfReg<CMD_IOCTLB0::Register> {
    DDRIO.reg(0x48b0 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_dllctl1(ch: u32) -> IosfReg<CMD_DLLCTL1::Register> {
    DDRIO.reg(0x48b4 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn cmd_pgovr(ch: u32) -> IosfReg<CMD_PGOVR::Register> {
    DDRIO.reg(0x48b8 + ch * CMD_CHANNEL_STRIDE)
}
pub const fn clkctl_dlltxctl(ch: u32) -> IosfReg<CLKCTL_DLLTXCTL::Register> {
    DDRIO.reg(0x5808 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_mdllctl(ch: u32) -> IosfReg<CLKCTL_MDLLCTL::Register> {
    DDRIO.reg(0x5810 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_dllpicoder0(ch: u32) -> IosfReg<CLKCTL_DLLPICODER0::Register> {
    DDRIO.reg(0x5820 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_dllpicoder1(ch: u32) -> IosfReg<CLKCTL_DLLPICODER1::Register> {
    DDRIO.reg(0x5824 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_ddr3resetctl(ch: u32) -> IosfReg<CLKCTL_DDR3RESETCTL::Register> {
    DDRIO.reg(0x5830 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_cfg_reg0(ch: u32) -> IosfReg<CLKCTL_CFG_REG0::Register> {
    DDRIO.reg(0x5838 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_cfg_reg1(ch: u32) -> IosfReg<CLKCTL_CFG_REG1::Register> {
    DDRIO.reg(0x5840 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_pointer_reg(ch: u32) -> IosfReg<CLKCTL_POINTER_REG::Register> {
    DDRIO.reg(0x5844 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_clkalign_reg2(ch: u32) -> IosfReg<CLKCTL_CLKALIGN_REG2::Register> {
    DDRIO.reg(0x5858 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_bonusclk_io(ch: u32) -> IosfReg<CLKCTL_BONUSCLK_IO::Register> {
    DDRIO.reg(0x589c + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_clkgate(ch: u32) -> IosfReg<CLKCTL_CLKGATE::Register> {
    DDRIO.reg(0x58ac + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_timing_ctrl(ch: u32) -> IosfReg<CLKCTL_TIMING_CTRL::Register> {
    DDRIO.reg(0x58b0 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_cmd_ptrctl(ch: u32) -> IosfReg<CLKCTL_CMD_PTRCTL::Register> {
    DDRIO.reg(0x58b4 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_cmd_cfg_reg2(ch: u32) -> IosfReg<CLKCTL_CMD_CFG_REG2::Register> {
    DDRIO.reg(0x58b8 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn clkctl_dllctl1(ch: u32) -> IosfReg<CLKCTL_DLLCTL1::Register> {
    DDRIO.reg(0x58c8 + ch * CLKCTL_CHANNEL_STRIDE)
}
pub const fn cmpctrl() -> IosfReg<CMPCTRL::Register> {
    DDRIO.reg(0x6800)
}
pub const fn nmscntrl() -> IosfReg<NMSCNTRL::Register> {
    DDRIO.reg(0x680c)
}
pub const fn latch1ctl() -> IosfReg<LATCH1CTL::Register> {
    DDRIO.reg(0x6814)
}
pub const fn tcocntctrl() -> IosfReg<TCOCNTCTRL::Register> {
    DDRIO.reg(0x683c)
}
pub const fn chnlbufstatic() -> IosfReg<CHNLBUFSTATIC::Register> {
    DDRIO.reg(0x68f0)
}
pub const fn dqvref(ch: u32) -> IosfReg<DQVREF::Register> {
    DDRIO.reg(0x6918 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn cmdvref(ch: u32) -> IosfReg<CMDVREF::Register> {
    DDRIO.reg(0x691c + ch * COMP_CHANNEL_STRIDE)
}
pub const fn clkvref(ch: u32) -> IosfReg<CLKVREF::Register> {
    DDRIO.reg(0x6920 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn dqsvref(ch: u32) -> IosfReg<DQSVREF::Register> {
    DDRIO.reg(0x6924 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn ctlvref(ch: u32) -> IosfReg<CTLVREF::Register> {
    DDRIO.reg(0x6928 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn tcovref(ch: u32) -> IosfReg<TCOVREF::Register> {
    DDRIO.reg(0x692c + ch * COMP_CHANNEL_STRIDE)
}
pub const fn dlysel(ch: u32) -> IosfReg<DLYSEL::Register> {
    DDRIO.reg(0x6930 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn tcodrambufodt(ch: u32) -> IosfReg<TCODRAMBUFODT::Register> {
    DDRIO.reg(0x6934 + ch * COMP_CHANNEL_STRIDE)
}
pub const fn compen4() -> IosfReg<COMPEN4::Register> {
    DDRIO.reg(0x6b28)
}
pub const fn mpllctrl() -> IosfReg<MPLLCTRL::Register> {
    DDRIO.reg(0x7800)
}
pub const fn mpllctrl1() -> IosfReg<MPLLCTRL1::Register> {
    DDRIO.reg(0x7808)
}
pub const fn mpllcsr0() -> IosfReg<MPLLCSR0::Register> {
    DDRIO.reg(0x7810)
}
pub const fn mpllcsr1() -> IosfReg<MPLLCSR1::Register> {
    DDRIO.reg(0x7814)
}
pub const fn mpllmisc() -> IosfReg<MPLLMISC::Register> {
    DDRIO.reg(0x7834)
}
pub const fn mplldftout() -> IosfReg<MPLLDFTOUT::Register> {
    DDRIO.reg(0x7858)
}
pub const fn masterrstn() -> IosfReg<MASTERRSTN::Register> {
    DDRIO.reg(0x7880)
}
pub const fn plllockdel() -> IosfReg<PLLLOCKDEL::Register> {
    DDRIO.reg(0x7884)
}
pub const fn sfrdel() -> IosfReg<SFRDEL::Register> {
    DDRIO.reg(0x7888)
}
