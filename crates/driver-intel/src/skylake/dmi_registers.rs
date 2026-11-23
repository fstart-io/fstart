// SPDX-License-Identifier: MIT OR Apache-2.0
//! DMI-only offsets/fields from sa_pre_mem_native.c:27–222,245–686.
//! Source SHA256: 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372.
use fstart_core::mmio::MmioReadWrite as RW;
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u64,
    pub SaMsrio [
        DATA OFFSET(0) NUMBITS(32) [],
        ADDRESS OFFSET(32) NUMBITS(32) []
    ]
];
register_bitfields![u16,
    pub LinkControl [
        ASPM OFFSET(0) NUMBITS(2) [],
        EXTENDED_SYNC OFFSET(4) NUMBITS(1) [],
        RETRAIN OFFSET(5) NUMBITS(1) []
    ],
    pub LinkStatus [
        SPEED OFFSET(0) NUMBITS(4) [],
        WIDTH OFFSET(4) NUMBITS(6) [],
        TRAINING OFFSET(11) NUMBITS(1) []
    ],
    pub LinkControl2 [
        SPEED OFFSET(0) NUMBITS(4) [],
        DEEMPHASIS OFFSET(6) NUMBITS(1) []
    ],
    pub VcStatus [ PENDING OFFSET(1) NUMBITS(1) [] ]
];
register_bitfields![u8, pub Opi [ INIT OFFSET(1) NUMBITS(1) [] ]];
register_bitfields![u32,
    pub Deemphasis [ SELECTABLE OFFSET(6) NUMBITS(1) [] ],
    pub Bundle1 [
        OFFCORGAIN OFFSET(10) NUMBITS(2) [],
        PGACQ OFFSET(16) NUMBITS(6) [],
        IGACQ OFFSET(22) NUMBITS(3) [],
        DFEIDACPD OFFSET(25) NUMBITS(4) []
    ],
    pub Bundle2 [
        TXEQCUR OFFSET(1) NUMBITS(7) [],
        TXEQPRECUR OFFSET(8) NUMBITS(7) [],
        TXEQPOSTCUR OFFSET(15) NUMBITS(7) [],
        G3BYPCOEFF OFFSET(23) NUMBITS(1) [],
        RXVCMDSSEL OFFSET(30) NUMBITS(2) []
    ],
    pub Bundle3 [
        PGTRK OFFSET(5) NUMBITS(6) [],
        RXRTBIN OFFSET(21) NUMBITS(4) [],
        TXRTBIN OFFSET(26) NUMBITS(4) []
    ],
    pub Bundle4 [
        G3RXCTLEPEAK OFFSET(6) NUMBITS(4) [],
        G2RXCTLEPEAK OFFSET(10) NUMBITS(4) [],
        SPARE OFFSET(25) NUMBITS(7) []
    ],
    pub BundleSpare [
        GEN2DFESUMADDMFC OFFSET(6) NUMBITS(2) [],
        GEN3DFESUMADDMFC OFFSET(9) NUMBITS(2) [],
        GEN3DFESUM_MFC OFFSET(13) NUMBITS(2) [],
        GEN2DFELSBSEL OFFSET(15) NUMBITS(2) [],
        GEN3DFELSBSEL OFFSET(17) NUMBITS(2) [],
        GEN2DFEIDACPD OFFSET(23) NUMBITS(3) [],
        GEN3DFEIDACPD OFFSET(27) NUMBITS(3) [],
        SPARE30 OFFSET(30) NUMBITS(1) []
    ],
    pub Common1 [
        PIBIASICTRLGEN3 OFFSET(14) NUMBITS(3) [],
        IBIAS OFFSET(29) NUMBITS(2) []
    ],
    pub Common2 [
        RXINCMSEL OFFSET(4) NUMBITS(6) [],
        VREFRXDET OFFSET(15) NUMBITS(5) []
    ],
    pub Lane0 [ G23RXVREF OFFSET(0) NUMBITS(5) [] ],
    pub Lane1 [
        G1RXVREFSEL OFFSET(4) NUMBITS(5) [],
        CDRPDDATMODE OFFSET(11) NUMBITS(1) []
    ],
    pub LaneTx [
        SWING OFFSET(8) NUMBITS(8) [],
        PI_RESSEL OFFSET(16) NUMBITS(6) [],
        OVERRIDE OFFSET(22) NUMBITS(1) []
    ],
    pub LaneIo [
        GEN1_GAIN OFFSET(6) NUMBITS(3) [],
        GEN2_GAIN OFFSET(9) NUMBITS(2) [],
        GEN3_GAIN OFFSET(12) NUMBITS(3) []
    ],
    pub Config5 [ UPCFGSLVTMR OFFSET(11) NUMBITS(4) [] ],
    pub EqConfig [
        EXTEIEOS OFFSET(0) NUMBITS(1) [],
        BYPADFSM OFFSET(1) NUMBITS(1) [],
        PHASE2_BYPASS OFFSET(15) NUMBITS(1) []
    ],
    pub EqPhase3 [
        COEFFICIENT OFFSET(0) NUMBITS(18) [],
        LATCH OFFSET(18) NUMBITS(1) [],
        LANE OFFSET(19) NUMBITS(2) []
    ],
    pub EqPreset [
        RP0 OFFSET(0) NUMBITS(4) [],
        EP0 OFFSET(8) NUMBITS(4) [],
        HINT0 OFFSET(12) NUMBITS(3) [],
        RP1 OFFSET(16) NUMBITS(4) [],
        EP1 OFFSET(24) NUMBITS(4) [],
        HINT1 OFFSET(28) NUMBITS(3) []
    ],
    pub Nfts [
        COUNTS OFFSET(0) NUMBITS(16) [],
        L0S_LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub TopologyControl [
        ENABLE OFFSET(0) NUMBITS(1) [],
        CID OFFSET(16) NUMBITS(8) [],
        PORT OFFSET(24) NUMBITS(8) []
    ],
    pub TopologyPortId [ CID OFFSET(16) NUMBITS(8) [] ],
    pub PowerTimer [ VALUE OFFSET(26) NUMBITS(6) [] ],
    pub DeviceCap [ MPS OFFSET(0) NUMBITS(3) [] ],
    pub DeviceControl [ MPS OFFSET(5) NUMBITS(3) [] ],
    pub VcControl [
        TC0 OFFSET(0) NUMBITS(1) [],
        MAP OFFSET(1) NUMBITS(7) [],
        ID OFFSET(24) NUMBITS(3) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    pub CapidA [ GEN2_DISABLED OFFSET(22) NUMBITS(1) [] ],
    pub CapidB [ GEN3_DISABLED OFFSET(15) NUMBITS(1) [] ]
];
register_structs! {
    pub TopologyLink {
        (0x00 => pub control: RW<u32, TopologyControl::Register>),
        (0x04 => _reserved),
        (0x08 => pub address: RW<u32>),
        (0x0c => pub address_high: RW<u32>),
        (0x10 => @END),
    },
    pub Bundle {
        (0x00 => pub cfg1: RW<u32, Bundle1::Register>),
        (0x04 => pub cfg2: RW<u32, Bundle2::Register>),
        (0x08 => pub cfg3: RW<u32, Bundle3::Register>),
        (0x0c => pub cfg4: RW<u32, Bundle4::Register>),
        (0x10 => _reserved),
        (0x18 => pub spare: RW<u32, BundleSpare::Register>),
        (0x1c => _tail),
        (0x20 => @END),
    },
    pub Lane {
        (0 => pub cfg0: RW<u32, Lane0::Register>),
        (4 => pub cfg1: RW<u32, Lane1::Register>),
        (8 => _reserved),
        (0x10 => @END),
    },
    pub Tx {
        (0 => pub control: RW<u32, LaneTx::Register>),
        (4 => _reserved),
        (0x10 => @END),
    },
    pub Io {
        (0 => pub control: RW<u32, LaneIo::Register>),
        (4 => _reserved),
        (0x10 => @END),
    },
    pub DmiRegisters {
        (0 => _reserved0),
        (0x04 => pub capability: RW<u32>),
        (0x08 => _capability_reserved),
        (0x14 => pub vc0: RW<u32, VcControl::Register>),
        (0x18 => _reserved1),
        (0x1a => pub vc0_status: RW<u16, VcStatus::Register>),
        (0x1c => _reserved2),
        (0x20 => pub vc1: RW<u32, VcControl::Register>),
        (0x24 => _reserved3),
        (0x26 => pub vc1_status: RW<u16, VcStatus::Register>),
        (0x28 => _reserved4),
        (0x38 => pub vcm: RW<u32, VcControl::Register>),
        (0x3c => _reserved5),
        (0x3e => pub vcm_status: RW<u16, VcStatus::Register>),
        (0x40 => _reserved6),
        (0x44 => pub topology_id: RW<u32, TopologyPortId::Register>),
        (0x48 => _topology_reserved0),
        (0x50 => pub topology_link: [TopologyLink; 2]),
        (0x70 => _topology_reserved1),
        (0x84 => pub link_cap: RW<u32>),
        (0x88 => pub link_control: RW<u16, LinkControl::Register>),
        (0x8a => pub link_status: RW<u16, LinkStatus::Register>),
        (0x8c => _reserved7),
        (0x98 => pub link_control2: RW<u16, LinkControl2::Register>),
        (0x9a => _reserved8),
        (0xa4 => pub device_cap: RW<u32, DeviceCap::Register>),
        (0xa8 => pub device_control: RW<u32, DeviceControl::Register>),
        (0xac => _reserved9),
        (0x1c4 => pub uncorrectable_status: fstart_core::mmio::MmioWriteOnly<u32>),
        (0x1c8 => _error_reserved0),
        (0x1d0 => pub correctable_status: fstart_core::mmio::MmioWriteOnly<u32>),
        (0x1d4 => _error_reserved1),
        (0x22c => pub nfts: RW<u32, Nfts::Register>),
        (0x230 => _reserved10),
        (0x25c => pub config5: RW<u32, Config5::Register>),
        (0x260 => _reserved11),
        (0x328 => pub pcie_lock2: RW<u32>),
        (0x32c => _pcie_lock_reserved),
        (0x384 => pub eq_phase3: RW<u32, EqPhase3::Register>),
        (0x388 => _reserved12),
        (0x70c => pub tx: [Tx; 4]),
        (0x74c => _reserved13),
        (0x808 => pub io: [Io; 4]),
        (0x848 => _reserved14),
        (0x904 => pub bundle: [Bundle; 2]),
        (0x944 => _reserved15),
        (0xa00 => pub lane: [Lane; 4]),
        (0xa40 => _reserved16),
        (0xa78 => pub opi: RW<u8, Opi::Register>),
        (0xa79 => _reserved17),
        (0xc04 => pub common1: RW<u32, Common1::Register>),
        (0xc08 => pub common2: RW<u32, Common2::Register>),
        (0xc0c => _reserved18),
        (0xc28 => pub power_timer: RW<u32, PowerTimer::Register>),
        (0xc2c => _reserved19),
        (0xd2c => pub peg_clock: RW<u32>),
        (0xd30 => _peg_clock_reserved),
        (0xd34 => pub iot: RW<u8>),
        (0xd35 => _iot_reserved),
        (0xd64 => pub si_version: [RW<u32>;2]),
        (0xd6c => _si_version_reserved),
        (0xda0 => pub eq_preset: [RW<u32, EqPreset::Register>; 2]),
        (0xda8 => _reserved20),
        (0xdd8 => pub eq_config: RW<u32, EqConfig::Register>),
        (0xddc => _reserved21),
        (0x1000 => @END),
    }
}
