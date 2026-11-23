// SPDX-License-Identifier: MIT OR Apache-2.0
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW};
use tock_registers::{register_bitfields, register_structs};
register_bitfields![u32,
    pub(super) Pcd [
        FUNCTIONS OFFSET(0) NUMBITS(16) [],
        DISABLE OFFSET(16) NUMBITS(4) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(super) Function [
        FUNCTION OFFSET(1) NUMBITS(3) [],
        DEVICE OFFSET(4) NUMBITS(5) []
    ],
    pub(super) Strap [ RPC OFFSET(14) NUMBITS(2) [] ],
    pub(super) Debug [
        SPCE OFFSET(5) NUMBITS(1) [],
        SQOL0 OFFSET(7) NUMBITS(1) [],
        CTONFAE OFFSET(14) NUMBITS(1) [],
        REVERSE OFFSET(4) NUMBITS(1) [],
        EXIT OFFSET(24) NUMBITS(2) []
    ],
    pub(super) Lcap [
        SPEED OFFSET(0) NUMBITS(4) [],
        WIDTH OFFSET(4) NUMBITS(6) [],
        ASPM OFFSET(10) NUMBITS(2) [],
        L0S OFFSET(10) NUMBITS(1) [],
        EL1 OFFSET(15) NUMBITS(3) [],
        PORT OFFSET(24) NUMBITS(8) []
    ],
    pub(super) SlotCap [
        HPS OFFSET(5) NUMBITS(1) [],
        HPC OFFSET(6) NUMBITS(1) [],
        POWER OFFSET(7) NUMBITS(8) [],
        SCALE OFFSET(15) NUMBITS(2) [],
        SLOT OFFSET(19) NUMBITS(13) []
    ],
    pub(super) LtrCap [
        ENABLE OFFSET(11) NUMBITS(1) [],
        OBFF OFFSET(18) NUMBITS(2) []
    ],
    pub(super) Mpc [
        CCEL OFFSET(15) NUMBITS(3) [],
        UCEL OFFSET(18) NUMBITS(3) [],
        LOCK OFFSET(23) NUMBITS(1) []
    ],
    pub(super) L1Cap [
        PCIPM12 OFFSET(0) NUMBITS(1) [],
        PCIPM11 OFFSET(1) NUMBITS(1) [],
        ASPM12 OFFSET(2) NUMBITS(1) [],
        ASPM11 OFFSET(3) NUMBITS(1) [],
        SUPPORTED OFFSET(4) NUMBITS(1) [],
        SCALE OFFSET(16) NUMBITS(2) [],
        VALUE OFFSET(19) NUMBITS(5) []
    ],
    pub(super) L1Ctl [
        PCI12 OFFSET(0) NUMBITS(1) [],
        ASPM12 OFFSET(2) NUMBITS(1) []
    ],
    pub(super) L1Ctl2 [
        SCALE OFFSET(0) NUMBITS(2) [],
        VALUE OFFSET(3) NUMBITS(5) []
    ],
    pub(super) Ccfg [
        UNRS OFFSET(4) NUMBITS(3) [],
        UNRD OFFSET(12) NUMBITS(2) [],
        UNSD OFFSET(23) NUMBITS(1) []
    ],
    pub(super) Mpc2 [
        EOI_DISABLE OFFSET(1) NUMBITS(1) [],
        LSTP OFFSET(6) NUMBITS(1) [],
        PTNFAE OFFSET(12) NUMBITS(1) []
    ],
    pub(super) E0 [
        DBC OFFSET(8) NUMBITS(1) [],
        DLC OFFSET(9) NUMBITS(1) [],
        SRDBC OFFSET(10) NUMBITS(1) [],
        BBCLKREQ OFFSET(12) NUMBITS(1) [],
        LCLKREQ OFFSET(13) NUMBITS(1) [],
        TRUNK OFFSET(14) NUMBITS(1) [],
        STATIC OFFSET(15) NUMBITS(1) [],
        SEOSC OFFSET(20) NUMBITS(1) []
    ],
    pub(super) Pwr [
        L1SQ OFFSET(1) NUMBITS(1) [],
        DBUPI OFFSET(15) NUMBITS(1) [],
        WPD OFFSET(17) NUMBITS(1) [],
        RETRAIN OFFSET(20) NUMBITS(1) []
    ],
    pub(super) F4 [
        PLL2 OFFSET(8) NUMBITS(1) [],
        PLL3 OFFSET(9) NUMBITS(1) [],
        SIID OFFSET(26) NUMBITS(2) [],
        SCPT OFFSET(30) NUMBITS(1) []
    ],
    pub(super) Pme [
        L1FSOE OFFSET(0) NUMBITS(1) [],
        L1LE OFFSET(17) NUMBITS(1) [],
        DLSULSD OFFSET(29) NUMBITS(1) [],
        DLSUPG OFFSET(30) NUMBITS(1) [],
        FDPPG OFFSET(31) NUMBITS(1) []
    ],
    pub(super) Pme2 [
        L1SC OFFSET(4) NUMBITS(1) [],
        DISSC OFFSET(5) NUMBITS(1) [],
        L23SC OFFSET(6) NUMBITS(1) [],
        DETSC OFFSET(7) NUMBITS(1) [],
        FDCP OFFSET(8) NUMBITS(1) [],
        PHY OFFSET(11) NUMBITS(1) []
    ],
    pub(super) Pce [
        PMC OFFSET(0) NUMBITS(1) [],
        HAE OFFSET(5) NUMBITS(1) []
    ],
    pub(super) Alc [
        BLOCK OFFSET(26) NUMBITS(1) [],
        ILLR OFFSET(28) NUMBITS(1) [],
        ITLR OFFSET(29) NUMBITS(1) []
    ],
    pub(super) Phy4 [ SQDIS OFFSET(27) NUMBITS(1) [] ],
    pub(super) LinkWord [
        DISABLE OFFSET(4) NUMBITS(1) [],
        ACTIVE OFFSET(29) NUMBITS(1) []
    ],
    pub(super) LinkState [ LTSSM OFFSET(24) NUMBITS(8) [] ],
    pub(super) Eq1 [
        TUPP OFFSET(1) NUMBITS(1) [],
        TS2 OFFSET(7) NUMBITS(1) [],
        COEFF_OVERRIDE OFFSET(15) NUMBITS(1) [],
        REMOTE_BYPASS OFFSET(16) NUMBITS(1) [],
        PHASE3_BYPASS OFFSET(17) NUMBITS(1) [],
        PHASE23_BYPASS OFFSET(18) NUMBITS(1) [],
        RESET_COUNTER OFFSET(23) NUMBITS(1) [],
        RECOVERY OFFSET(24) NUMBITS(8) []
    ],
    pub(super) Eq2 [
        ERRORS OFFSET(0) NUMBITS(8) [],
        SEARCH OFFSET(12) NUMBITS(4) [],
        EVAL OFFSET(16) NUMBITS(4) []
    ],
    pub(super) Haeq [
        MAC OFFSET(19) NUMBITS(1) [],
        COUNT OFFSET(28) NUMBITS(4) []
    ],
    pub(super) Coeff [ PCM OFFSET(31) NUMBITS(1) [] ],
    pub(super) Counter [
        LANE OFFSET(0) NUMBITS(2) [],
        ERRORS OFFSET(8) NUMBITS(24) []
    ],
    pub(super) Dfe [ TAP3 OFFSET(10) NUMBITS(1) [] ],
    pub(super) DfeOverride [
        T0 OFFSET(24) NUMBITS(1) [],
        T1 OFFSET(26) NUMBITS(1) [],
        T2 OFFSET(28) NUMBITS(1) [],
        T3 OFFSET(30) NUMBITS(1) []
    ],
    pub(super) Vga [ OVERRIDE OFFSET(4) NUMBITS(1) [] ],
    pub(super) Tap [ UPDATE OFFSET(3) NUMBITS(1) [] ],
    pub(super) Pidac [ VALID OFFSET(16) NUMBITS(1) [] ],
    pub(super) Jim [
        MARGIN OFFSET(0) NUMBITS(1) [],
        TIMER_HIGH OFFSET(20) NUMBITS(2) [],
        MODE OFFSET(28) NUMBITS(1) [],
        ENABLE OFFSET(29) NUMBITS(1) [],
        COARSE OFFSET(30) NUMBITS(1) []
    ],
    pub(super) Fia [
        CLOCK OFFSET(15) NUMBITS(3) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(super) Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ],
    pub(super) Grants [ COUNT OFFSET(0) NUMBITS(5) [] ],
    pub(super) Flow [
        INGRESS OFFSET(1) NUMBITS(1) [],
        EGRESS OFFSET(2) NUMBITS(1) []
    ],
    pub(super) DmiFlow [
        BIT7 OFFSET(7) NUMBITS(1) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    pub(super) Flags [ SLOT OFFSET(8) NUMBITS(1) [] ],
    pub(super) Dcap [ PAYLOAD OFFSET(0) NUMBITS(3) [] ],
    pub(super) Dctl [ PAYLOAD OFFSET(5) NUMBITS(3) [] ],
    pub(super) Dsta [ CORRECTABLE OFFSET(0) NUMBITS(1) [] ],
    pub(super) LinkCtl [
        DISABLE OFFSET(4) NUMBITS(1) [],
        RETRAIN OFFSET(5) NUMBITS(1) [],
        COMMON OFFSET(6) NUMBITS(1) []
    ],
    pub(super) LinkSta [
        SPEED OFFSET(0) NUMBITS(4) [],
        WIDTH OFFSET(4) NUMBITS(6) [],
        TRAINING OFFSET(11) NUMBITS(1) [],
        SLOT_CLOCK OFFSET(12) NUMBITS(1) [],
        ACTIVE OFFSET(13) NUMBITS(1) []
    ],
    pub(super) SlotCtl [
        PDE OFFSET(3) NUMBITS(1) [],
        HPE OFFSET(5) NUMBITS(1) []
    ],
    pub(super) SlotSta [
        CHANGE OFFSET(3) NUMBITS(1) [],
        PRESENT OFFSET(6) NUMBITS(1) []
    ],
    pub(super) Dev2 [
        LTR OFFSET(10) NUMBITS(1) [],
        OBFF OFFSET(13) NUMBITS(2) []
    ],
    pub(super) Link2 [ SPEED OFFSET(0) NUMBITS(4) [] ],
    pub(super) LinkSta2 [ EQ_COMPLETE OFFSET(1) NUMBITS(1) [] ]
];
register_bitfields![u8,
    pub(super) MpcLow [
        HPME OFFSET(1) NUMBITS(1) [],
        MCTP OFFSET(3) NUMBITS(1) []
    ],
    pub(super) MpcMid [ SPEED OFFSET(5) NUMBITS(2) [] ],
    pub(super) MpcHigh [
        IRRCE OFFSET(1) NUMBITS(1) [],
        MMBNCE OFFSET(3) NUMBITS(1) [],
        HOTPLUG OFFSET(6) NUMBITS(1) []
    ],
    pub(super) Phy2 [ WAIT OFFSET(4) NUMBITS(4) [] ],
    pub(super) Rppgen [ PTOTOP OFFSET(6) NUMBITS(1) [] ]
];
register_structs! {
    pub(super) Registers {
        (0 => _r0), (0x42 => pub flags:RW<u16,Flags::Register>),
        (0x44 => pub dcap:RW<u16,Dcap::Register>), (0x46 => _r1),
        (0x48 => pub dctl:RW<u16,Dctl::Register>), (0x4a => pub dsta:RW<u16,Dsta::Register>),
        (0x4c => pub lcap:RW<u32,Lcap::Register>), (0x50 => pub lctl:RW<u16,LinkCtl::Register>), (0x52 => pub lsta:RO<u16,LinkSta::Register>),
        (0x54 => pub slotcap:RW<u32,SlotCap::Register>), (0x58 => pub slotctl:RW<u16,SlotCtl::Register>), (0x5a => pub slotsta:RW<u16,SlotSta::Register>),
        (0x5c => _r2), (0x64 => pub ltrcap:RW<u32,LtrCap::Register>), (0x68 => pub dev2:RW<u16,Dev2::Register>), (0x6a => _r3),
        (0x70 => pub link2:RW<u16,Link2::Register>), (0x72 => pub linksta2:RO<u16,LinkSta2::Register>), (0x74 => _r4),
        (0xd0 => pub ccfg:RW<u32,Ccfg::Register>), (0xd4 => pub mpc2:RW<u32,Mpc2::Register>),
        (0xd8 => pub mpc_low:RW<u8,MpcLow::Register>), (0xd9 => pub mpc_mid:RW<u8,MpcMid::Register>), (0xda => _r5), (0xdb => pub mpc_high:RW<u8,MpcHigh::Register>),
        (0xdc => _r6), (0xe2 => pub rppgen:RW<u8,Rppgen::Register>), (0xe3 => _r7), (0xe8 => pub pwr:RW<u32,Pwr::Register>),
        (0xec => _r8), (0xf5 => pub phy2:RW<u8,Phy2::Register>), (0xf6 => _r9),
        (0x100 => pub aer:RW<u32>), (0x104 => _r10), (0x108 => pub uem:RW<u32>), (0x10c => _r11), (0x110 => pub correctable:RW<u32>), (0x114 => _r12),
        (0x140 => pub acs:RW<u32>), (0x144 => pub acscap:RW<u32>), (0x148 => _r13),
        (0x200 => pub l1:RW<u32>), (0x204 => pub l1cap:RW<u32,L1Cap::Register>), (0x208 => pub l1ctl:RW<u32,L1Ctl::Register>), (0x20c => pub l1ctl2:RW<u32,L1Ctl2::Register>), (0x210 => _r14),
        (0x220 => pub secondary:RW<u32>), (0x224 => pub lctl3:RW<u32>), (0x228 => pub lane_error:RW<u32>), (0x22c => pub presets:[RW<u16>;4]),
        (0x234 => _r15), (0x300 => pub rtp1:RW<u32>), (0x304 => pub rtp2:RW<u32>), (0x308 => _r16),
        (0x314 => pub nfts:RW<u32>), (0x318 => pub l0sc:RW<u32>), (0x31c => _r17), (0x320 => pub cfg2:RW<u32>), (0x324 => pub debug:RW<u32,Debug::Register>),
        (0x328 => pub state:RO<u32,LinkState::Register>), (0x32c => pub clear_recovery:RW<u32>), (0x330 => _r18), (0x338 => pub alc:RW<u32,Alc::Register>),
        (0x33c => _r19), (0x424 => pub pme2:RW<u32,Pme2::Register>), (0x428 => _r20),
        (0x450 => pub eq1:RW<u32,Eq1::Register>), (0x454 => pub coeff:[RW<u32,Coeff::Register>;2]), (0x45c => _r21),
        (0x468 => pub haeq:RW<u32,Haeq::Register>), (0x46c => _r22), (0x478 => pub g3l0:RW<u32>), (0x47c => pub eq2:RW<u32,Eq2::Register>), (0x480 => pub counter:RW<u32,Counter::Register>), (0x484 => @END),
    }
}
