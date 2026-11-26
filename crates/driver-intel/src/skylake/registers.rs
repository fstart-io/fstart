// SPDX-License-Identifier: MIT OR Apache-2.0
//! MCHBAR register layout from native/mc_registers.h, native_init.h,
//! mc_timing.c and address_decode.c; auxiliary decodes from soc/systemagent.h.
//! Offsets are SKL/KBL, not GM45 aliases.

use super::{phy_registers::*, reut_registers::*};
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
};

/// Ordinary REUT register pair: the native controller setup accesses the low
/// dword first, then the high dword. Do not use this for FIFO transactions.
#[repr(C)]
pub struct ReutDwordPair<R: RegisterLongName = ()> {
    low: MmioReadWrite<u32>,
    high: MmioReadWrite<u32>,
    fields: core::marker::PhantomData<R>,
}

impl<R: RegisterLongName> Readable for ReutDwordPair<R> {
    type T = u64;
    type R = R;

    fn get(&self) -> u64 {
        u64::from(self.low.get()) | (u64::from(self.high.get()) << 32)
    }
}

impl<R: RegisterLongName> Writeable for ReutDwordPair<R> {
    type T = u64;
    type R = R;

    fn set(&self, value: u64) {
        self.low.set(value as u32);
        self.high.set((value >> 32) as u32);
    }
}

pub const MCHBAR_BASE: usize = 0xfed1_0000;
pub const MCHBAR_SIZE: usize = 0x8000;

register_bitfields![u32,
    // systemagent_early.c defines only enable bit 0 for these special BARs.
    // Address masks/high-bit and lock semantics are deliberately not inferred.
    pub SaDecode [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    // Native systemagent_early.c publishes these two bits together for memory/PM.
    pub BiosResetComplete [ MEMORY_AND_PM OFFSET(0) NUMBITS(2) [] ],
    pub SaPmControl [ PM_SYNC OFFSET(9) NUMBITS(2) [] ],
    // Each package-limit DWORD; LOCK is meaningful in the HIGH DWORD only.
    pub MchPowerLimit [
        POWER OFFSET(0) NUMBITS(15) [],
        ENABLE OFFSET(15) NUMBITS(1) [],
        CLAMP OFFSET(16) NUMBITS(1) [],
        WINDOW OFFSET(17) NUMBITS(7) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub BiosRequest [
        RATIO OFFSET(0) NUMBITS(4) [],
        REFCLK_100 OFFSET(4) NUMBITS(1) [],
        RATIO_MSB OFFSET(5) NUMBITS(1) [],
        ODD_RATIO OFFSET(8) NUMBITS(1) [],
        RUN_BUSY OFFSET(31) NUMBITS(1) []
    ],
    pub BiosData [
        RATIO OFFSET(0) NUMBITS(4) [],
        RATIO_MSB OFFSET(5) NUMBITS(1) [],
        ODD_RATIO OFFSET(8) NUMBITS(1) []
    ],
    pub Compensation [ DONE OFFSET(16) NUMBITS(1) [] ],
    pub MadInter [
        DDR_TYPE OFFSET(0) NUMBITS(2) [
            Ddr4=0,
            Ddr3=1,
            Lpddr3=2
        ],
        CH_L_MAP OFFSET(4) NUMBITS(1) [],
        CH_S_SIZE OFFSET(12) NUMBITS(7) []
    ],
    pub MadDimm [
        L_SIZE OFFSET(0) NUMBITS(6) [],
        L_WIDTH OFFSET(8) NUMBITS(2) [],
        L_RANKS OFFSET(10) NUMBITS(1) [],
        S_SIZE OFFSET(16) NUMBITS(6) [],
        S_WIDTH OFFSET(24) NUMBITS(2) [],
        S_RANKS OFFSET(26) NUMBITS(1) []
    ],
    pub MadIntra [
        DIMM_L_MAP OFFSET(0) NUMBITS(1) [],
        RANK_INTERLEAVE OFFSET(4) NUMBITS(1) [],
        ENHANCED_INTERLEAVE OFFSET(8) NUMBITS(1) [],
        ECC OFFSET(12) NUMBITS(2) []
    ],
    pub ChannelHash [
        MASK OFFSET(6) NUMBITS(14) [],
        LSB OFFSET(24) NUMBITS(3) [],
        ENABLE OFFSET(28) NUMBITS(1) []
    ],
    pub MrParams [
        RANK_WIDTHS OFFSET(0) NUMBITS(8) [],
        MR4_PERIOD OFFSET(8) NUMBITS(16) [],
        DDR4_TS_READOUT OFFSET(24) NUMBITS(1) []
    ],
    pub ReutGlobalError [
        ERRORS OFFSET(0) NUMBITS(2) [],
        DONE OFFSET(16) NUMBITS(2) []
    ],
    pub ReutSequence [
        DUMMY_READS OFFSET(7) NUMBITS(1) [],
        SUBSEQ_START OFFSET(24) NUMBITS(3) [],
        SUBSEQ_END OFFSET(28) NUMBITS(3) [],
        GLOBAL_CONTROL OFFSET(11) NUMBITS(1) [],
        INIT_MODE OFFSET(12) NUMBITS(2) [
            Testing=1,
            Mrs=2,
            Normal=3
        ]
    ],
    pub ReutControl [
        START OFFSET(0) NUMBITS(1) [],
        STOP OFFSET(1) NUMBITS(1) [],
        STOP_ON_ANY_ERROR OFFSET(4) NUMBITS(1) [],
        CLEAR_ERRORS OFFSET(2) NUMBITS(1) []
    ],
    pub CadbMrs [
        GAP OFFSET(0) NUMBITS(3) [],
        GAP_SCALE OFFSET(7) NUMBITS(1) [],
        END OFFSET(16) NUMBITS(3) [],
        WAIT_SCALE OFFSET(28) NUMBITS(1) [],
        WAIT OFFSET(29) NUMBITS(3) []
    ],
    pub CadbWritePointer [ WRITE_ENABLE OFFSET(4) NUMBITS(1) [] ],
    pub OdtControl [
        MPR_TRAIN OFFSET(31) NUMBITS(1) [],
        OVERRIDE OFFSET(0) NUMBITS(4) []
    ],
    pub ControlRanksUsed [ DIS_ODT_STATIC OFFSET(4) NUMBITS(2) [] ],
    pub InitState [
        MRC_DONE OFFSET(0) NUMBITS(1) [],
        REFRESH_ENABLE OFFSET(3) NUMBITS(1) [],
        DONE_ACK OFFSET(5) NUMBITS(1) [],
        MRC_DONE2 OFFSET(7) NUMBITS(1) [],
        DCLK_ENABLE OFFSET(22) NUMBITS(1) [],
        IO_RESET OFFSET(24) NUMBITS(1) []
    ],
    pub CkeControl [
        OVERRIDE OFFSET(0) NUMBITS(4) [],
        ON OFFSET(16) NUMBITS(4) []
    ],
    pub TcPre [
        TRP OFFSET(0) NUMBITS(6) [],
        TRAS OFFSET(8) NUMBITS(8) [],
        TRTP OFFSET(16) NUMBITS(6) [],
        TWRPRE OFFSET(24) NUMBITS(8) []
    ],
    pub TcAct [
        TFAW OFFSET(0) NUMBITS(6) [],
        TRRD_L OFFSET(8) NUMBITS(4) [],
        TRRD_S OFFSET(12) NUMBITS(4) [],
        DERATING OFFSET(16) NUMBITS(2) []
    ],
    pub TcPowerDown [
        TCKE OFFSET(0) NUMBITS(4) [],
        TXP OFFSET(4) NUMBITS(4) [],
        TXPDLL OFFSET(8) NUMBITS(6) [],
        TPRPDEN OFFSET(14) NUMBITS(2) [],
        TRDPDEN OFFSET(16) NUMBITS(7) [],
        TWRPDEN OFFSET(24) NUMBITS(7) []
    ],
    /// native/odt_power.c profile-1 recipes: eight opaque nibbles, not an impedance encoding.
    pub OdtMatrix [
        ENTRY0 OFFSET(0) NUMBITS(4) [],
        ENTRY1 OFFSET(4) NUMBITS(4) [],
        ENTRY2 OFFSET(8) NUMBITS(4) [],
        ENTRY3 OFFSET(12) NUMBITS(4) [],
        ENTRY4 OFFSET(16) NUMBITS(4) [],
        ENTRY5 OFFSET(20) NUMBITS(4) [],
        ENTRY6 OFFSET(24) NUMBITS(4) [],
        ENTRY7 OFFSET(28) NUMBITS(4) []
    ],
    // TC_RDRD uses a six-bit SG; TC_WRRD alone has a seven-bit SG.
    pub ReadReadTurnaround [
        SG OFFSET(0) NUMBITS(6) [],
        DG OFFSET(8) NUMBITS(6) [],
        DR OFFSET(16) NUMBITS(6) [],
        DD OFFSET(24) NUMBITS(6) []
    ],
    pub Turnaround [
        SG OFFSET(0) NUMBITS(7) [],
        DG OFFSET(8) NUMBITS(6) [],
        DR OFFSET(16) NUMBITS(6) [],
        DD OFFSET(24) NUMBITS(6) []
    ],
    pub TcOdt [
        READ_DURATION OFFSET(0) NUMBITS(3) [],
        READ_DELAY OFFSET(4) NUMBITS(3) [],
        WRITE_DURATION OFFSET(8) NUMBITS(1) [],
        TCL OFFSET(16) NUMBITS(5) [],
        TCWL OFFSET(21) NUMBITS(5) [],
        TAONPD OFFSET(26) NUMBITS(5) []
    ],
    pub Scheduler [
        TECHNOLOGY OFFSET(0) NUMBITS(2) [],
        COMMAND_STRETCH OFFSET(2) NUMBITS(2) [],
        N_TO_ONE OFFSET(4) NUMBITS(3) [],
        ADDRESS_MIRROR OFFSET(8) NUMBITS(2) [],
        X8_DEVICE OFFSET(10) NUMBITS(2) [],
        CPDED OFFSET(12) NUMBITS(3) [],
        COMMAND_THREE_STATE OFFSET(23) NUMBITS(1) [],
        CLOCK_TO_CKE OFFSET(24) NUMBITS(3) [],
        ENABLE_ODT_MATRIX OFFSET(27) NUMBITS(1) []
    ],
    pub RefreshPolicy [
        LOW_WATERMARK OFFSET(0) NUMBITS(8) [],
        HIGH_WATERMARK OFFSET(8) NUMBITS(4) [],
        PANIC_WATERMARK OFFSET(12) NUMBITS(4) [],
        TREFI_X9 OFFSET(25) NUMBITS(7) []
    ],
    pub RefreshTiming [
        TREFI OFFSET(0) NUMBITS(16) [],
        TRFC OFFSET(16) NUMBITS(10) []
    ],
    pub SelfRefreshTiming [
        TXSDLL OFFSET(0) NUMBITS(12) [],
        TXS_OFFSET OFFSET(12) NUMBITS(4) [],
        TZQOPER OFFSET(16) NUMBITS(10) [],
        TMOD OFFSET(26) NUMBITS(5) []
    ],
    pub ZqCalibration [
        PERIOD OFFSET(0) NUMBITS(10) [],
        TZQCS OFFSET(10) NUMBITS(10) []
    ],
    pub Mr2Shadow [
        MR2 OFFSET(0) NUMBITS(14) [],
        SRT_AVAILABLE OFFSET(14) NUMBITS(2) []
    ],
    pub WriteDelay [
        RANK0 OFFSET(0) NUMBITS(2) [],
        RANK1 OFFSET(2) NUMBITS(2) [],
        RANK2 OFFSET(4) NUMBITS(2) [],
        RANK3 OFFSET(6) NUMBITS(2) [],
        DEC_WRD OFFSET(31) NUMBITS(1) []
    ],
    pub DecodeCbit [
        DIS_SINGLE_CHANNEL_SR OFFSET(4) NUMBITS(1) [],
        DIS_OTHER_CHANNEL_STOLEN_REFRESH OFFSET(5) NUMBITS(1) []
    ],
    pub DecodeMisc [ SPARE OFFSET(0) NUMBITS(5) [] ],
    pub RefreshStagger [ DISPLAY_ENABLE OFFSET(14) NUMBITS(1) [] ],
    pub RoundtripLatency [
        RANK0 OFFSET(0) NUMBITS(7) [],
        RANK1 OFFSET(8) NUMBITS(7) [],
        RANK2 OFFSET(16) NUMBITS(7) [],
        RANK3 OFFSET(24) NUMBITS(7) []
    ],
    pub IoLatency [
        RANK0 OFFSET(0) NUMBITS(4) [],
        RANK1 OFFSET(4) NUMBITS(4) [],
        RANK2 OFFSET(8) NUMBITS(4) [],
        RANK3 OFFSET(12) NUMBITS(4) [],
        IO_COMP OFFSET(16) NUMBITS(7) [],
        RFR OFFSET(24) NUMBITS(7) []
    ],
    pub SchedulerCbit [
        DIS_PT_IT OFFSET(6) NUMBITS(1) [],
        DIS_ODT OFFSET(12) NUMBITS(1) []
    ],
    pub SchedulerSecondCbit [
        DIS_SRX_MR4 OFFSET(0) NUMBITS(1) [],
        DIS_BURST_REFRESH_LIMIT OFFSET(2) NUMBITS(1) []
    ],
    pub MaintenanceSpare [ FORCE_X2REF OFFSET(8) NUMBITS(1) [] ],
    pub Pcit [
        CPU OFFSET(0) NUMBITS(8) [],
        GT OFFSET(16) NUMBITS(8) []
    ],
    pub MrCommand [
        ADDRESS OFFSET(0) NUMBITS(8) [],
        DATA OFFSET(8) NUMBITS(8) [],
        RANK OFFSET(16) NUMBITS(2) [],
        COMMAND OFFSET(18) NUMBITS(2) [],
        DEVICE_SKIP OFFSET(20) NUMBITS(9) [],
        ASSUME_IDLE OFFSET(29) NUMBITS(1) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    pub DriverGate [
        BIT10 OFFSET(10) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    pub HdaAudioRouting [
        DEVICE_NUMBER OFFSET(3) NUMBITS(5) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    // The native source names this opaque control only by its offset and bit.
    pub SaPreMemControl [ BIT22 OFFSET(22) NUMBITS(1) [] ]
];
register_bitfields![u8,
    pub PowerAwareInterrupts [ PRIORITY OFFSET(0) NUMBITS(3) [Fixed = 4] ]
];
register_bitfields![u64,
    pub CadbProgram [
        ADDRESS OFFSET(0) NUMBITS(17) [],
        BANK OFFSET(24) NUMBITS(4) [],
        CHIP_SELECT OFFSET(32) NUMBITS(4) [],
        COMMAND OFFSET(40) NUMBITS(4) [],
        ODT OFFSET(48) NUMBITS(4) [],
        CKE OFFSET(56) NUMBITS(4) []
    ],
    pub WdbWatermarks [
        LOW OFFSET(0) NUMBITS(8) [],
        HIGH OFFSET(8) NUMBITS(8) [],
        PENDING OFFSET(16) NUMBITS(8) [],
        WRITE_CAS OFFSET(24) NUMBITS(8) [],
        READ_CAS OFFSET(32) NUMBITS(8) [],
        READ_VC1 OFFSET(40) NUMBITS(8) [],
        WRITE_STARVATION OFFSET(48) NUMBITS(8) []
    ],
    pub Qos [
        ISO_WINDOW OFFSET(0) NUMBITS(17) [],
        WRITE_STARVATION_WINDOW OFFSET(17) NUMBITS(11) [],
        READ_STARVATION_WINDOW OFFSET(28) NUMBITS(11) [],
        VC0_READ_COUNT OFFSET(39) NUMBITS(9) [],
        VC1_READ_STARVATION OFFSET(48) NUMBITS(1) [],
        WRITE_STARVATION_ISO OFFSET(49) NUMBITS(1) [],
        READ_STARVATION_ISO OFFSET(50) NUMBITS(1) [],
        DIS_VC0_COUNTER OFFSET(51) NUMBITS(1) []
    ]
];

register_structs! {
    pub ControlChannelRegisters {
        (0x00 => _reserved0),
        (0x18 => pub rank_pi:MmioReadWrite<u32,RankPi::Register>),
        (0x1c => pub control:MmioReadWrite<u32,FubControl::Register>),
        (0x20 => pub ranks_used:MmioReadWrite<u32,ControlRanksUsed::Register>),
        (0x24 => _reserved1),
        (0x100 => @END),
    },
    pub ChannelRegisters {
        (0x000 => pub tc_pre: MmioReadWrite<u32,TcPre::Register>),
        (0x004 => pub tc_act: MmioReadWrite<u32,TcAct::Register>),
        (0x008 => pub tc_power_down: MmioReadWrite<u32,TcPowerDown::Register>),
        (0x00c => pub tc_rdrd: MmioReadWrite<u32,Turnaround::Register>),
        (0x010 => pub tc_rdwr: MmioReadWrite<u32,Turnaround::Register>),
        (0x014 => pub tc_wrrd: MmioReadWrite<u32,Turnaround::Register>),
        (0x018 => pub tc_wrwr: MmioReadWrite<u32,Turnaround::Register>),
        (0x01c => pub scheduler: MmioReadWrite<u32,Scheduler::Register>),
        (0x020 => pub roundtrip_latency: MmioReadWrite<u32,RoundtripLatency::Register>),
        (0x024 => pub io_latency: MmioReadWrite<u32,IoLatency::Register>),
        (0x028 => pub sched_cbit: MmioReadWrite<u32,SchedulerCbit::Register>),
        (0x02c => pub sched_second_cbit: MmioReadWrite<u32,SchedulerSecondCbit::Register>),
        (0x030 => _reserved0),
        (0x03c => pub pcit: MmioReadWrite<u32,Pcit::Register>),
        (0x040 => pub channel_power_down: MmioReadWrite<u32>),
        (0x044 => _pcit_reserved),
        (0x070 => pub tc_odt: MmioReadWrite<u32,TcOdt::Register>),
        (0x074 => _reserved1),
        (0x080 => pub odt_matrix: MmioReadWrite<u32,OdtMatrix::Register>),
        (0x084 => _odt_matrix_reserved),
        (0x100 => pub wdb_mux: MmioReadWrite<u32,WdbMux::Register>),
        (0x104 => _wdb_reserved0),
        (0x108 => pub wdb_write_seed: [MmioReadWrite<u32>;3]),
        (0x114 => pub wdb_read_seed: [MmioReadWrite<u32>;3]),
        (0x120 => _wdb_reserved1),
        (0x138 => pub wdb_lmn: MmioReadWrite<u32,Lmn::Register>),
        (0x13c => _wdb_reserved2),
        (0x144 => pub wdb_invert: MmioReadWrite<u32>),
        (0x148 => _wdb_reserved3),
        (0x190 => pub cke: MmioReadWrite<u32,CkeControl::Register>),
        (0x194 => pub odt: MmioReadWrite<u32,OdtControl::Register>),
        (0x198 => pub cadb_control: MmioReadWrite<u32,CadbControl::Register>),
        (0x19c => pub cadb_mrs: MmioReadWrite<u32,CadbMrs::Register>),
        (0x1a0 => pub cadb_mux: MmioReadWrite<u32,CadbMux::Register>),
        (0x1a4 => pub cadb_seed: [MmioReadWrite<u32>;3]),
        (0x1b0 => _cadb_reserved0),
        (0x1bc => pub cadb_lmn: MmioReadWrite<u32,Lmn::Register>),
        (0x1c0 => pub cadb_write_pointer: MmioReadWrite<u32,CadbWritePointer::Register>),
        (0x1c4 => _cadb_reserved1),
        (0x1c8 => pub cadb_program: MmioReadWrite<u64,CadbProgram::Register>),
        (0x1d0 => pub cadb_loop_count: MmioReadWrite<u32>),
        (0x1d4 => _cadb_reserved2),
        (0x200 => pub wdb_control: MmioReadWrite<u32,WdbControl::Register>),
        (0x204 => _wdb_control_reserved),
        (0x210 => pub mr_params: MmioReadWrite<u32,MrParams::Register>),
        (0x214 => pub mr_command: MmioReadWrite<u32,MrCommand::Register>),
        (0x218 => _reserved3a),
        (0x228 => pub ddr4_mpr_temp: MmioReadWrite<u32>),
        (0x22c => _reserved3b),
        (0x238 => pub refresh_policy: MmioReadWrite<u32,RefreshPolicy::Register>),
        (0x23c => pub refresh_timing: MmioReadWrite<u32,RefreshTiming::Register>),
        (0x240 => pub self_refresh_timing: MmioReadWrite<u32,SelfRefreshTiming::Register>),
        (0x244 => pub refresh_stagger: MmioReadWrite<u32,RefreshStagger::Register>),
        (0x248 => pub zq_calibration: MmioReadWrite<u32,ZqCalibration::Register>),
        (0x24c => pub mr2_shadow: MmioReadWrite<u32,Mr2Shadow::Register>),
        (0x250 => _reserved4),
        (0x254 => pub init_state: MmioReadWrite<u32>),
        (0x258 => _rank_reserved),
        (0x260 => pub energy: [MmioReadWrite<u32>;5]),
        (0x274 => _energy_reserved),
        (0x278 => pub write_delay: MmioReadWrite<u32,WriteDelay::Register>),
        (0x27c => _reserved5),
        (0x290 => pub ldat_pdat: MmioReadWrite<u32,LdatPdat::Register>),
        (0x294 => pub ldat_sdat: MmioReadWrite<u32,LdatSdat::Register>),
        (0x298 => _ldat_reserved),
        (0x29c => pub ldat_data: [MmioReadWrite<u32>;2]),
        (0x2a4 => _ldat_reserved1),
        (0x300 => pub error_count: [MmioReadOnly<u32,ErrorCounterStatus::Register>;9]),
        (0x324 => _error_reserved0),
        (0x330 => pub error_control: MmioReadWrite<u32,ErrorControl::Register>),
        (0x334 => pub error_ecc_mask: MmioReadWrite<u32>),
        (0x338 => pub error_data_mask: ReutDwordPair),
        (0x340 => pub error_data: MmioReadOnly<u64>),
        (0x348 => pub error_detail: MmioReadOnly<u64,ErrorDetail::Register>),
        (0x350 => _error_reserved1),
        (0x360 => pub error_counter_control: [MmioReadWrite<u32,ErrorCounterControl::Register>;9]),
        (0x384 => _error_reserved2),
        (0x3a0 => pub mr_content: [MmioReadWrite<u32>;4]),
        (0x3b0 => _mr_content_reserved),
        (0x3b8 => pub mr6_vref: [[MmioReadWrite<u32>;2];4]),
        (0x3d8 => _mr_vref_reserved),
        (0x3fc => pub maintenance_spare: MmioReadWrite<u32,MaintenanceSpare::Register>),
        (0x400 => @END),
    },
    pub MchRegisters {
        (0x0000 => pub data_lane: [[DataLaneRegisters;2];9]),
        (0x1200 => pub cke_channel: [CkeChannelRegisters;2]),
        (0x1400 => pub command_north: [CommandChannelRegisters;2]),
        (0x1600 => _reserved0),
        (0x1800 => pub clock_channel: [ClockChannelRegisters;2]),
        (0x1a00 => pub command_south: [CommandChannelRegisters;2]),
        (0x1c00 => pub control_channel:[ControlChannelRegisters;2]),
        (0x1e00 => _control_reserved),
        (0x2000 => pub scramble: [MmioReadWrite<u32>;2]),
        (0x2008 => pub ddr_misc: MmioReadWrite<u32,DdrMisc::Register>),
        (0x200c => _scramble_reserved),
        (0x3000 => pub data_channel: [DataLaneRegisters;2]),
        (0x3200 => pub command_channel: [CommandChannelRegisters;2]),
        (0x3400 => pub cke_control_channel: [CkeControlChannelRegisters;2]),
        (0x3600 => pub data_multicast: DataLaneRegisters),
        (0x3700 => _data_reserved),
        (0x3724 => pub command_dll_pi: MmioReadWrite<u32,FubDllPi::Register>),
        (0x3728 => _command_reserved),
        (0x3824 => pub cke_control_dll_pi: MmioReadWrite<u32,FubDllPi::Register>),
        (0x3828 => _cke_reserved),
        (0x3914 => pub clock_dll_pi: MmioReadWrite<u32,FubDllPi::Register>),
        (0x3918 => _clock_mc_reserved),
        (0x3920 => pub clock_vsshi: MmioReadWrite<u32,ClockVsshi::Register>),
        (0x3924 => _clock_vsshi_reserved),
        (0x3a00 => pub data_comp0: MmioReadOnly<u32,DataComp0::Register>),
        (0x3a04 => pub data_comp1: MmioReadOnly<u32,DataComp1::Register>),
        (0x3a08 => pub command_comp: MmioReadOnly<u32,CommandComp::Register>),
        (0x3a0c => pub control_comp: MmioReadOnly<u32,CommandComp::Register>),
        (0x3a10 => pub clock_comp: MmioReadOnly<u32,ClockComp::Register>),
        (0x3a14 => pub comp_control0: MmioReadWrite<u32,CompControl0::Register>),
        (0x3a18 => pub comp_control1: MmioReadWrite<u32,CompControl1::Register>),
        (0x3a1c => pub comp_vsshi: MmioReadWrite<u32,VsshiComp::Register>),
        (0x3a20 => pub comp_override: MmioReadWrite<u32,CompOverride::Register>),
        (0x3a24 => pub comp_vsshi_control: MmioReadWrite<u32,VsshiControl::Register>),
        (0x3a28 => pub comp_control2: MmioReadWrite<u32,CompControl2::Register>),
        (0x3a2c => _phy_reserved),
        (0x4000 => pub channel: [ChannelRegisters;2]),
        (0x4800 => pub reut_global_control: MmioReadWrite<u32,ReutControl::Register>),
        (0x4804 => pub reut_global_error: MmioReadOnly<u32,ReutGlobalError::Register>),
        (0x4808 => pub reut_subsequence0: [MmioReadWrite<u32,Subsequence::Register>;8]),
        (0x4828 => _subseq_reserved),
        (0x4830 => pub reut_subsequence1: [MmioReadWrite<u32,Subsequence::Register>;8]),
        (0x4850 => _reut_reserved0),
        (0x48a8 => pub reut_sequence: [[MmioReadWrite<u32,ReutSequence::Register>;2];2]),
        (0x48b8 => pub reut_control: [MmioReadWrite<u32,ReutControl::Register>;2]),
        (0x48c0 => _reut_reserved1),
        (0x48d8 => pub reut_address_start: [ReutDwordPair<TestAddress::Register>;2]),
        (0x48e8 => pub reut_address_wrap: [ReutDwordPair<TestAddress::Register>;2]),
        (0x48f8 => pub reut_address_current: [MmioReadOnly<u64,TestAddress::Register>;2]),
        (0x4908 => pub reut_address_order: [MmioReadWrite<u32,AddressOrder::Register>;2]),
        (0x4910 => pub reut_address_increment: [ReutDwordPair<AddressIncrement::Register>;2]),
        (0x4920 => _address_reserved),
        (0x4930 => pub reut_rank_map: [MmioReadWrite<u32>;2]),
        (0x4938 => _rank_map_reserved),
        (0x4958 => pub reut_dummy_mask: [MmioReadWrite<u32>;2]),
        (0x4960 => pub reut_dummy_control: [MmioReadWrite<u32,Lmn::Register>;2]),
        (0x4968 => _dummy_reserved),
        (0x4980 => pub reut_bank_map: [[MmioReadWrite<u32>;2];2]),
        (0x4990 => _reserved2),
        (0x49a0 => pub reut_loop_count: [MmioReadWrite<u32>;2]),
        (0x49a8 => _loop_reserved),
        (0x4c28 => pub sched_cbit: MmioReadWrite<u32,SchedulerCbit::Register>),
        (0x4c2c => pub sched_second_cbit: MmioReadWrite<u32,SchedulerSecondCbit::Register>),
        (0x4c30 => _reserved3),
        (0x4c3c => pub sched_pcit: MmioReadWrite<u32,Pcit::Register>),
        (0x4c40 => pub power_down_config: MmioReadWrite<u32>),
        (0x4c44 => _reserved4),
        (0x4c68 => pub sched_wdbwm: [MmioReadWrite<u32>;2]),
        (0x4c70 => _reserved5),
        (0x4d90 => pub cke_multicast: MmioReadWrite<u32,CkeControl::Register>),
        (0x4d94 => pub odt_multicast: MmioReadWrite<u32,OdtControl::Register>),
        (0x4d98 => _reserved6),
        (0x5000 => pub mad_inter: MmioReadWrite<u32,MadInter::Register>),
        (0x5004 => pub mad_intra: [MmioReadWrite<u32,MadIntra::Register>;2]),
        (0x500c => pub mad_dimm: [MmioReadWrite<u32,MadDimm::Register>;2]),
        (0x5014 => _reserved7),
        (0x5018 => pub decode_misc: MmioReadWrite<u32,DecodeMisc::Register>),
        (0x501c => pub decode_cbit: MmioReadWrite<u32,DecodeCbit::Register>),
        (0x5020 => _reserved8),
        (0x5024 => pub channel_hash: MmioReadWrite<u32,ChannelHash::Register>),
        (0x5028 => _reserved9),
        (0x5030 => pub init_state: MmioReadWrite<u32,InitState::Register>),
        (0x5034 => pub mrc_revision: MmioReadWrite<u32>),
        (0x5038 => _reserved10),
        (0x5060 => pub self_refresh_config: MmioReadWrite<u32>),
        (0x5064 => _self_refresh_reserved),
        (0x5074 => pub stall_drain: MmioReadWrite<u32>),
        (0x5078 => _stall_reserved),
        (0x5084 => pub rcomp_timer: MmioReadOnly<u32,Compensation::Register>),
        (0x5088 => _reserved11),
        (0x50fc => pub mc_lock: MmioReadWrite<u32>),
        (0x5100 => _mc_lock_reserved),
        (0x5130 => pub sched_qos: [MmioReadWrite<u32>;2]),
        (0x5138 => pub driver_gate: MmioReadWrite<u32,DriverGate::Register>),
        (0x513c => _reserved12),
        (0x5400 => pub gfx_vtd_bar: MmioReadWrite<u32,SaDecode::Register>),
        (0x5404 => pub gfx_vtd_bar_high: MmioReadWrite<u32>),
        (0x5408 => pub edram_bar: MmioReadWrite<u32,SaDecode::Register>),
        (0x540c => pub edram_bar_high: MmioReadWrite<u32>),
        (0x5410 => pub vtvc0_bar: MmioReadWrite<u32,SaDecode::Register>),
        (0x5414 => pub vtvc0_bar_high: MmioReadWrite<u32>),
        (0x5418 => pub power_aware_interrupts: MmioReadWrite<u8,PowerAwareInterrupts::Register>),
        (0x5419 => _sa_decode_reserved),
        (0x5420 => pub gdxc_bar: MmioReadWrite<u32,SaDecode::Register>),
        (0x5424 => pub gdxc_bar_high: MmioReadWrite<u32>),
        (0x5428 => _sa_decode_reserved_end),
        (0x5500 => pub pavp_message: MmioReadWrite<u32>),
        (0x5504 => _pavp_reserved),
        (0x5880 => pub ddr_ptm: MmioReadWrite<u32>),
        (0x5884 => pub energy_scale: MmioReadWrite<u32>),
        (0x5888 => pub channel_power_floor: MmioReadWrite<u32>),
        (0x588c => _thermal_reserved),
        (0x5890 => pub thermal_threshold: [MmioReadWrite<u32>;4]),
        (0x58a0 => _thermal_threshold_reserved),
        (0x58a4 => pub ddr_voltage: MmioReadWrite<u32>),
        (0x58a8 => _voltage_reserved),
        (0x58d0 => pub thermal_budget: [MmioReadWrite<u32>;4]),
        (0x58e0 => pub rapl_limit: [MmioReadWrite<u32>;2]),
        (0x58e8 => _rapl_reserved),
        (0x59a0 => pub package_power_limit: [MmioReadWrite<u32,MchPowerLimit::Register>;2]),
        (0x59a8 => _package_power_reserved),
        (0x59b8 => pub odt_power_saving: MmioReadWrite<u32,OdtPowerSavingControl::Register>),
        (0x59bc => _odt_power_reserved),
        (0x5d10 => pub scratchpad: MmioReadWrite<u64>),
        (0x5d18 => _reserved13),
        (0x5da0 => pub mailbox_data: MmioReadWrite<u32,DllBandwidth::Register>),
        (0x5da4 => pub mailbox_interface: MmioReadWrite<u32,MailboxInterface::Register>),
        (0x5da8 => pub bios_reset_complete: MmioReadWrite<u32,BiosResetComplete::Register>),
        (0x5dac => _mailbox_reserved),
        (0x5e00 => pub bios_request: MmioReadWrite<u32,BiosRequest::Register>),
        (0x5e04 => pub bios_data: MmioReadOnly<u32,BiosData::Register>),
        (0x5e08 => _reserved14),
        (0x5f00 => pub sa_pm_control: MmioReadWrite<u32,SaPmControl::Register>),
        (0x5f04 => _sa_pm_reserved),
        (0x5f08 => pub comp_pcu: MmioReadWrite<u32,CompPcu::Register>),
        (0x5f0c => _comp_pcu_reserved),
        (0x6008 => pub hdaudio_routing: MmioReadWrite<u32,HdaAudioRouting::Register>),
        (0x600c => _hdaudio_reserved),
        (0x6020 => pub remap_base_lock: MmioReadWrite<u32>),
        (0x6024 => _remap_lock_reserved),
        (0x6030 => pub remap_base_readback: MmioReadWrite<u32>),
        (0x6034 => pub remap_limit_readback: MmioReadWrite<u32>),
        (0x6038 => _remap_readback_reserved),
        (0x63fc => pub vtd_track_lock: MmioReadWrite<u32>),
        (0x6400 => _vtd_track_reserved),
        (0x6800 => pub edram_lock: MmioReadWrite<u32>),
        (0x6804 => _edram_lock_reserved),
        (0x7000 => pub bios_reset_lock: MmioReadWrite<u32>),
        (0x7004 => _bios_reset_lock_reserved),
        (0x701c => pub sa_pre_mem_control: MmioReadWrite<u32,SaPreMemControl::Register>),
        (0x7020 => _sa_pre_mem_reserved),
        (0x7060 => pub edram_pm0: MmioReadWrite<u32>),
        (0x7064 => _edram_pm_reserved0),
        (0x7070 => pub edram_pm1: MmioReadWrite<u32>),
        (0x7074 => _edram_pm_reserved1),
        (0x7080 => pub edram_pm2: MmioReadWrite<u32>),
        (0x7084 => _edram_pm_reserved2),
        (0x7400 => pub peg_credit0: [MmioReadWrite<u32>;8]),
        (0x7420 => _peg_credit_reserved0),
        (0x7454 => pub peg_credit1: [MmioReadWrite<u32>;4]),
        (0x7464 => _peg_credit_reserved1),
        (0x746c => pub peg_credit2: MmioReadWrite<u32>),
        (0x7470 => _peg_credit_reserved2),
        (0x77fc => pub ipu_vtd_lock: MmioReadWrite<u32>),
        (0x7800 => _ipu_vtd_lock_reserved),
        (0x7bfc => pub itt_vtd_lock: MmioReadWrite<u32>),
        (0x7c00 => _itt_vtd_lock_reserved),
        (0x8000 => @END),
    }
}

impl MchRegisters {
    /// # Safety
    /// The whole 32 KiB MCHBAR must be mapped at `base`, owned exclusively by
    /// the caller for `'a`, and programmed/enabled in host-bridge config space.
    /// Individual routines document any PLL/RCOMP sequencing prerequisites.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert_eq!(base & (MCHBAR_SIZE - 1), 0);
        unsafe { &*(base as *const Self) }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    #[repr(C, align(32768))]
    pub struct Image(pub [core::cell::UnsafeCell<u32>; MCHBAR_SIZE / 4]);
    pub fn image() -> Box<Image> {
        Box::new(Image(
            [const { core::cell::UnsafeCell::new(0) }; MCHBAR_SIZE / 4],
        ))
    }
    impl Image {
        pub fn set(&self, offset: usize, value: u32) {
            assert_eq!(offset % 4, 0);
            // SAFETY: the simulated registers use interior-mutable storage.
            unsafe {
                self.0[offset / 4].get().write(value);
            }
        }
        pub fn registers(&self) -> &MchRegisters {
            // SAFETY: aligned in-memory register image, owned for this borrow.
            unsafe { MchRegisters::at(self.0.as_ptr() as usize) }
        }
    }
}

register_bitfields![u32,
    /// memory_map_native.c:55–57: unused OCLA high word lock only.
    pub GdxcOclaHigh [ LOCK OFFSET(2) NUMBITS(1) [] ]
];
register_structs! {
    pub GdxcRegisters {
        (0x00 => _unused_prefix),
        (0x24 => pub ocla_address_high: MmioReadWrite<u32,GdxcOclaHigh::Register>),
        (0x28 => @END),
    }
}
impl GdxcRegisters {
    /// # Safety
    /// Own the whole mapped 4 KiB GDXC window exclusively for this lifetime.
    /// Its operational MCH decode must correspond to this mapping (virtual
    /// aliases are permitted). Only the unused native OCLA lock is supported.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert_eq!(base & 0xfff, 0);
        unsafe { &*(base as *const Self) }
    }
}
