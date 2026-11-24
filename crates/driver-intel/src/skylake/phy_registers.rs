// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDRIO register models from native/mc_registers.h and mc_reg.c.
//! Unicast lanes stride by 0x200, channels by 0x100; multicast and
//! channel-broadcast apertures are distinct hardware operations.

use super::reut_registers::TrainingFeedback;
use fstart_core::mmio::MmioReadWrite;
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32,
    pub DataOffsetComp [
        DRIVE_UP OFFSET(0) NUMBITS(6) [],
        DRIVE_DOWN OFFSET(6) NUMBITS(6) [],
        ODT_UP OFFSET(12) NUMBITS(5) [],
        ODT_DOWN OFFSET(17) NUMBITS(5) [],
        TCO OFFSET(22) NUMBITS(5) [],
        SLEW OFFSET(27) NUMBITS(5) []
    ],
    pub DataComp0 [
        DRIVE_UP OFFSET(0) NUMBITS(6) [],
        DRIVE_DOWN OFFSET(9) NUMBITS(6) [],
        SLEW OFFSET(20) NUMBITS(5) [],
        TCO OFFSET(26) NUMBITS(6) []
    ],
    pub CommandComp [
        SLEW OFFSET(0) NUMBITS(6) [],
        DRIVE_UP OFFSET(12) NUMBITS(6) [],
        DRIVE_DOWN OFFSET(18) NUMBITS(6) []
    ],
    pub ClockComp [
        DRIVE_UP OFFSET(11) NUMBITS(6) [],
        DRIVE_DOWN OFFSET(17) NUMBITS(6) []
    ],
    pub DataOffsetTrain [
        TX_DQ OFFSET(12) NUMBITS(6) [],
        TX_DQS OFFSET(18) NUMBITS(6) [],
        RX_DQS OFFSET(6) NUMBITS(6) [],
        VREF OFFSET(24) NUMBITS(7) []
    ],
    pub RxTrain [
        RCVEN OFFSET(0) NUMBITS(9) [],
        DQS_P OFFSET(9) NUMBITS(6) [],
        EQ OFFSET(15) NUMBITS(5) [],
        DQS_N OFFSET(20) NUMBITS(6) [],
        VREF OFFSET(26) NUMBITS(6) []
    ],
    pub TxTrain [
        DQ OFFSET(0) NUMBITS(9) [],
        DQS OFFSET(9) NUMBITS(9) [],
        EQ OFFSET(20) NUMBITS(6) []
    ],
    pub DataControl0 [
        WL_TRAIN OFFSET(1) NUMBITS(1) [],
        TX_PI_ON OFFSET(7) NUMBITS(1) [],
        RX_TRAIN OFFSET(0) NUMBITS(1) [],
        RL_TRAIN OFFSET(2) NUMBITS(1) [],
        SENSE_AMP_TRAIN OFFSET(3) NUMBITS(1) [],
        FORCE_ODT OFFSET(24) NUMBITS(1) [],
        READ_PREAMBLE OFFSET(29) NUMBITS(1) [],
        VDDQ_HIGH OFFSET(19) NUMBITS(1) [],
        READ_RF_READ OFFSET(20) NUMBITS(1) [],
        READ_RF_WRITE OFFSET(21) NUMBITS(1) [],
        READ_RF_RANK OFFSET(22) NUMBITS(2) [],
        DISABLE_STATIC_ODT OFFSET(26) NUMBITS(1) []
    ],
    pub DataControl1 [
        DLL_MASK OFFSET(4) NUMBITS(2) [],
        RX_BIAS OFFSET(10) NUMBITS(4) [],
        ODT_DELAY OFFSET(14) NUMBITS(4) [],
        ODT_DURATION OFFSET(18) NUMBITS(3) [],
        SENSE_AMP_DELAY OFFSET(21) NUMBITS(4) [],
        SENSE_AMP_DURATION OFFSET(25) NUMBITS(3) []
    ],
    pub DataControl2 [
        WL_LONG_DELAY OFFSET(18) NUMBITS(1) [],
        FORCE_BIAS OFFSET(5) NUMBITS(1) [],
        FORCE_RX OFFSET(6) NUMBITS(1) [],
        LEAKER_COMP OFFSET(7) NUMBITS(2) [],
        RX_STAGGER OFFSET(0) NUMBITS(5) [],
        RX_DQS_AMP OFFSET(9) NUMBITS(4) [],
        RX_CLOCK_STAGGER OFFSET(13) NUMBITS(5) [],
        DDR4 OFFSET(20) NUMBITS(1) [],
        VDDQ_ODT OFFSET(21) NUMBITS(1) [],
        VTT_ODT OFFSET(22) NUMBITS(1) [],
        CONSTANT_Z_TX OFFSET(23) NUMBITS(1) [],
        RX_VREF_PMFC OFFSET(25) NUMBITS(1) [],
        CBEN OFFSET(26) NUMBITS(3) [],
        TX_DESKEW_DISABLE OFFSET(29) NUMBITS(1) [],
        RX_DESKEW_DISABLE OFFSET(30) NUMBITS(1) [],
        SLEW_BYPASS OFFSET(31) NUMBITS(1) []
    ],
    pub VsshiControl [
        TARGET OFFSET(0) NUMBITS(6) [],
        ENABLE OFFSET(6) NUMBITS(1) [],
        PNC_ENABLE OFFSET(8) NUMBITS(1) [],
        BW_DIVIDER OFFSET(14) NUMBITS(2) [],
        VTT_ODT OFFSET(16) NUMBITS(1) [],
        PANIC OFFSET(18) NUMBITS(4) [],
        GAIN_BOOST OFFSET(22) NUMBITS(1) []
    ],
    pub VsshiComp [
        PANIC_VREF OFFSET(0) NUMBITS(6) [],
        VT_OFFSET OFFSET(6) NUMBITS(5) [],
        VT_SLOPE_A OFFSET(11) NUMBITS(3) [],
        VT_SLOPE_B OFFSET(14) NUMBITS(3) [],
        PANIC_UP OFFSET(17) NUMBITS(7) [],
        PANIC_DOWN OFFSET(24) NUMBITS(7) []
    ],
    pub VrefControl [
        BW_DIVIDER OFFSET(2) NUMBITS(2) [],
        HI_BW OFFSET(4) NUMBITS(1) [],
        SLOW_BW OFFSET(8) NUMBITS(1) [],
        SLOPE_BOOST OFFSET(10) NUMBITS(1) [],
        VT_SLOPE OFFSET(12) NUMBITS(6) [],
        OUTPUT_PANIC OFFSET(18) NUMBITS(5) []
    ],
    pub VrefAdjust [
        CA OFFSET(0) NUMBITS(7) [],
        CH1 OFFSET(7) NUMBITS(7) [],
        CA_SLOW_BW OFFSET(29) NUMBITS(1) [],
        CH1_SLOW_BW OFFSET(31) NUMBITS(1) [],
        ENABLE_CA OFFSET(21) NUMBITS(1) [],
        ENABLE_CH1 OFFSET(22) NUMBITS(1) [],
        ENABLE_CH0 OFFSET(23) NUMBITS(1) [],
        HIZ_TIMER OFFSET(24) NUMBITS(2) [],
        VDDQ_HIGH OFFSET(26) NUMBITS(1) []
    ],
    pub VttGen [
        PANIC OFFSET(4) NUMBITS(4) [],
        VSSHI_COMP OFFSET(16) NUMBITS(1) []
    ],
    pub FubControl [
        REF_PI OFFSET(0) NUMBITS(4) [],
        DLL_MASK OFFSET(4) NUMBITS(2) [],
        CMD_TX_EQ OFFSET(13) NUMBITS(2) [],
        EARLY_WEAK_DRIVE OFFSET(15) NUMBITS(2) [],
        CTL_TX_EQ OFFSET(17) NUMBITS(2) [],
        CTL_SR_DRIVE OFFSET(19) NUMBITS(2) [],
        VDDQ_HIGH OFFSET(27) NUMBITS(1) []
    ],
    pub RankPi [
        RANK0 OFFSET(0) NUMBITS(7) [],
        RANK1 OFFSET(7) NUMBITS(7) [],
        RANK2 OFFSET(14) NUMBITS(7) [],
        RANK3 OFFSET(21) NUMBITS(7) []
    ],
    pub CommandCompOffset [
        SLEW OFFSET(0) NUMBITS(5) [],
        DRIVE_UP OFFSET(9) NUMBITS(4) [],
        DRIVE_DOWN OFFSET(13) NUMBITS(4) []
    ],
    pub OdtPowerSavingControl [
        READ OFFSET(0) NUMBITS(8) [],
        WRITE OFFSET(8) NUMBITS(8) [],
        COMMAND OFFSET(16) NUMBITS(8) []
    ],
    pub ClockCompOffset [
        DRIVE_UP OFFSET(8) NUMBITS(4) [],
        DRIVE_DOWN OFFSET(12) NUMBITS(4) []
    ],
    pub DdrMisc [
        FORCE_COMP_UPDATE OFFSET(5) NUMBITS(1) [],
        WAKE_CYCLES OFFSET(0) NUMBITS(2) [],
        SLEEP_CYCLES OFFSET(2) NUMBITS(3) [],
        WEAK_LOCK_LATENCY OFFSET(6) NUMBITS(4) [],
        NO_CHANNEL_INTERLEAVE OFFSET(10) NUMBITS(1) [],
        DDR4 OFFSET(20) NUMBITS(1) [],
        DATA_CLOCK_GATE_DISABLE OFFSET(23) NUMBITS(1) []
    ],
    pub CompControl0 [
        DISABLE_STATIC_ODT OFFSET(3) NUMBITS(1) [],
        ODT_UP_DOWN OFFSET(4) NUMBITS(6) [],
        FIX_ODT_DOWN OFFSET(10) NUMBITS(1) [],
        DQ_DRIVE_VREF OFFSET(11) NUMBITS(4) [],
        DQ_ODT_VREF OFFSET(15) NUMBITS(5) [],
        CMD_DRIVE_VREF OFFSET(20) NUMBITS(4) [],
        CTL_DRIVE_VREF OFFSET(24) NUMBITS(4) [],
        CLK_DRIVE_VREF OFFSET(28) NUMBITS(4) []
    ],
    pub CompControl1 [
        DQ_CELLS OFFSET(0) NUMBITS(4) [],
        DQ_CYCLE OFFSET(4) NUMBITS(1) [],
        CMD_CELLS OFFSET(5) NUMBITS(4) [],
        CMD_CYCLE OFFSET(9) NUMBITS(1) [],
        CTL_CELLS OFFSET(10) NUMBITS(4) [],
        CTL_CYCLE OFFSET(14) NUMBITS(1) [],
        CLK_CELLS OFFSET(15) NUMBITS(4) [],
        CLK_CYCLE OFFSET(19) NUMBITS(1) [],
        VDDQ_HIGH OFFSET(25) NUMBITS(1) [],
        VDDQ_LOW OFFSET(26) NUMBITS(1) [],
        DQ_STATIC_LEGS OFFSET(27) NUMBITS(2) []
    ],
    pub CompControl2 [
        VDDQ_ODT OFFSET(0) NUMBITS(1) [],
        VTT_ODT OFFSET(1) NUMBITS(1) [],
        CBEN OFFSET(17) NUMBITS(3) [],
        CMD_DRIVE_VREF200 OFFSET(20) NUMBITS(4) []
    ],
    pub DataComp1 [
        ODT_UP OFFSET(0) NUMBITS(6) [],
        ODT_DOWN OFFSET(26) NUMBITS(6) []
    ],
    pub CompOverride [ LS_COMP OFFSET(24) NUMBITS(3) [] ],
    pub CompPcu [
        DISABLE OFFSET(0) NUMBITS(1) [],
        FORCE OFFSET(8) NUMBITS(1) []
    ],
    pub DataDllPi [ PHASE_DRIVER_POWER_SAVE OFFSET(31) NUMBITS(1) [] ],
    pub FubDllPi [ PHASE_DRIVER_POWER_SAVE OFFSET(25) NUMBITS(1) [] ],
    pub ClockVsshi [ VTT_COMP_FOR_VSSHI OFFSET(16) NUMBITS(1) [] ],
    // native_init.h SET_EPG_BIOS_POWER_OVERHEAD_{0,1}_CMD differ at bit 8.
    // No wider mailbox parameter layout is assumed here.
    pub MailboxInterface [
        COMMAND_STATUS OFFSET(0) NUMBITS(8) [],
        EPG_OVERHEAD_INDEX OFFSET(8) NUMBITS(1) [],
        RUN_BUSY OFFSET(31) NUMBITS(1) []
    ],
    pub DllBandwidth [
        MT1067 OFFSET(0) NUMBITS(3) [],
        MT1333 OFFSET(3) NUMBITS(3) [],
        MT1600 OFFSET(6) NUMBITS(3) [],
        MT1867 OFFSET(9) NUMBITS(3) []
    ]
];

register_structs! {
    pub DataLaneRegisters {
        (0x00 => pub rx_train: [MmioReadWrite<u32,RxTrain::Register>;4]),
        (0x10 => pub rx_per_bit: [MmioReadWrite<u32>;4]),
        (0x20 => pub tx_train: [MmioReadWrite<u32,TxTrain::Register>;4]),
        (0x30 => pub tx_per_bit: [MmioReadWrite<u32>;4]),
        (0x40 => _reserved0),
        (0x48 => pub tx_crosstalk: MmioReadWrite<u32>),
        (0x4c => _reserved1),
        (0x50 => pub rx_offset_vdq: [MmioReadWrite<u32>;4]),
        (0x60 => pub feedback: MmioReadWrite<u32,TrainingFeedback::Register>),
        (0x64 => pub dll_pi: MmioReadWrite<u32,DataDllPi::Register>),
        (0x68 => pub offset_comp: MmioReadWrite<u32,DataOffsetComp::Register>),
        (0x6c => pub control1: MmioReadWrite<u32,DataControl1::Register>),
        (0x70 => pub control2: MmioReadWrite<u32,DataControl2::Register>),
        (0x74 => pub vref_control: MmioReadWrite<u32,VrefControl::Register>),
        (0x78 => pub vsshi_control: MmioReadWrite<u32,VsshiControl::Register>),
        (0x7c => pub offset_train: MmioReadWrite<u32,DataOffsetTrain::Register>),
        (0x80 => pub control0: MmioReadWrite<u32,DataControl0::Register>),
        (0x84 => pub vref_adjust: MmioReadWrite<u32,VrefAdjust::Register>),
        (0x88 => _reserved3),
        (0x8c => pub vtt_gen: MmioReadWrite<u32,VttGen::Register>),
        (0x90 => _reserved4),
        (0x100 => @END),
    },
    pub CkeChannelRegisters {
        (0x00 => _reserved0),
        (0x08 => pub cmd_pi: MmioReadWrite<u32,RankPi::Register>),
        (0x0c => _reserved1),
        (0x18 => pub control_pi: MmioReadWrite<u32,RankPi::Register>),
        (0x1c => pub control: MmioReadWrite<u32,FubControl::Register>),
        (0x20 => pub ranks_used: MmioReadWrite<u32>),
        (0x24 => _reserved2),
        (0x100 => @END),
    },
    pub ClockChannelRegisters {
        (0x00 => pub ranks_used: MmioReadWrite<u32>),
        (0x04 => _reserved0),
        (0x08 => pub comp_offset: MmioReadWrite<u32,ClockCompOffset::Register>),
        (0x0c => pub rank_pi: MmioReadWrite<u32,RankPi::Register>),
        (0x10 => pub control: MmioReadWrite<u32,FubControl::Register>),
        (0x14 => _reserved1),
        (0x100 => @END),
    },
    pub CommandChannelRegisters {
        (0x00 => _reserved0),
        (0x04 => pub comp_offset: MmioReadWrite<u32,CommandCompOffset::Register>),
        (0x08 => pub cmd_pi: MmioReadWrite<u32,RankPi::Register>),
        (0x0c => pub control: MmioReadWrite<u32,FubControl::Register>),
        (0x10 => _reserved1),
        (0x100 => @END),
    },
    pub CkeControlChannelRegisters {
        (0x00 => _reserved0),
        (0x14 => pub comp_offset: MmioReadWrite<u32>),
        (0x18 => pub rank_pi: MmioReadWrite<u32,RankPi::Register>),
        (0x1c => _reserved1),
        (0x100 => @END),
    }
}
