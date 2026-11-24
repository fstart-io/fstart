// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::southbridge::xhci::PORTSC;
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW};
use tock_registers::{register_bitfields, register_structs};
register_bitfields![u32,
    pub(super) Global [
        VBUS OFFSET(22) NUMBITS(1) [],
        PORT OFFSET(25) NUMBITS(1) []
    ],
    pub(super) Word [ VALUE OFFSET(0) NUMBITS(32) [] ],
    pub(super) Pll [
        FORCE OFFSET(26) NUMBITS(1) [],
        LOCK OFFSET(27) NUMBITS(1) []
    ],
    pub(super) Comp [
        BIT9 OFFSET(9) NUMBITS(1) [],
        BIT10 OFFSET(10) NUMBITS(1) []
    ],
    pub(super) Afe1 [
        TX OFFSET(8) NUMBITS(3) [],
        PRE OFFSET(11) NUMBITS(3) [],
        HALF OFFSET(14) NUMBITS(1) [],
        HIGH OFFSET(26) NUMBITS(6) []
    ],
    pub(super) Afe2 [ EMP OFFSET(23) NUMBITS(2) [] ],
    pub(super) Utmi [
        BIT7 OFFSET(7) NUMBITS(1) [],
        BIT9 OFFSET(9) NUMBITS(1) [],
        BIT10 OFFSET(10) NUMBITS(1) [],
        BIT11 OFFSET(11) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) []
    ],
    pub(super) Mipi [
        BIT3 OFFSET(3) NUMBITS(1) [],
        BIT5 OFFSET(5) NUMBITS(1) []
    ],
    pub(super) Tx [ VALUE OFFSET(16) NUMBITS(6) [] ],
    pub(super) TxCalc [ INIT OFFSET(1) NUMBITS(1) [] ],
    pub(super) Hcs1 [ PORTS OFFSET(24) NUMBITS(8) [] ],
    pub(super) Command [
        RUN OFFSET(0) NUMBITS(1) [],
        RESET OFFSET(1) NUMBITS(1) []
    ],
    pub(super) Status [
        HALTED OFFSET(0) NUMBITS(1) [],
        NOT_READY OFFSET(11) NUMBITS(1) []
    ],
    pub(super) SsPorts [
        OFFSET OFFSET(0) NUMBITS(8) [],
        COUNT OFFSET(8) NUMBITS(8) []
    ],
    pub(super) Hcc [ XECP OFFSET(16) NUMBITS(16) [] ],
    pub(super) Pdo [
        USB2 OFFSET(0) NUMBITS(15) [],
        USB3 OFFSET(0) NUMBITS(10) []
    ],
    pub(super) SsEnable [
        PORTS OFFSET(0) NUMBITS(10) [],
        BIT30 OFFSET(30) NUMBITS(1) []
    ]
];
register_bitfields![u16, pub(super) Power [
    STATE OFFSET(0) NUMBITS(2) [],
    PME_STATUS OFFSET(15) NUMBITS(1) []
],
    pub(super) Pce [
        BIT2 OFFSET(2) NUMBITS(1) [],
        BIT3 OFFSET(3) NUMBITS(1) []
    ] ];
register_bitfields![u8,
    pub(super) Config1 [ BIT0 OFFSET(0) NUMBITS(1) [] ],
    pub(super) Config2 [
        BIT2 OFFSET(2) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) [],
        BIT5 OFFSET(5) NUMBITS(1) [],
        STATUS OFFSET(7) NUMBITS(1) []
    ],
    pub(super) PowerByte [ STATE OFFSET(0) NUMBITS(2) [] ]
];
register_structs! {
    pub(super) PortRegisters { (0 => pub status:RW<u32,PORTSC::Register>), (4 => _reserved), (0x10 => @END), },
    pub(super) Registers {
        (0 => pub caplength:RO<u8>), (1 => _reserved0),
        (4 => pub hcs1:RW<u32,Hcs1::Register>), (8 => _reserved1),
        (0xc => pub hcs3:RW<u32>), (0x10 => pub hcc:RO<u32,Hcc::Register>), (0x14 => _reserved2),
        (0x80 => pub command:RW<u32,Command::Register>), (0x84 => pub status:RO<u32,Status::Register>), (0x88 => _reserved3),
        (0x480 => pub ports:[PortRegisters;26]), (0x620 => _reserved4),
        (0x8008 => pub usb2_protocol:RW<u32>), (0x800c => _reserved5),
        (0x8028 => pub usb3_protocol:RO<u32,SsPorts::Register>), (0x802c => _reserved6),
        (0x8094 => pub scheduler:RW<u32>), (0x8098 => _reserved7),
        (0x809c => pub idma:RW<u32>), (0x80a0 => _reserved8),
        (0x80a4 => pub pmctrl:RW<u32>), (0x80a8 => pub pgcbctrl:RW<u32>), (0x80ac => _reserved9),
        (0x80b0 => pub misc:RW<u32>), (0x80b4 => pub misc2:RW<u32>), (0x80b8 => pub ss_enable:RW<u32,SsEnable::Register>), (0x80bc => _reserved10),
        (0x80c0 => pub aux_ctrl:RW<u32>), (0x80c4 => _reserved10b),
        (0x80c8 => pub bw_hs:RW<u32>), (0x80cc => _reserved11),
        (0x80e0 => pub aux1:RW<u32>), (0x80e4 => _reserved12),
        (0x80ec => pub port_link:RW<u32>), (0x80f0 => pub link1:RW<u32>), (0x80f4 => _reserved13),
        (0x80fc => pub link4:RW<u32>), (0x8100 => _reserved14),
        (0x8110 => pub trm2:RW<u32>), (0x8114 => _reserved15),
        (0x8128 => pub bw_max:RW<u32>), (0x812c => _reserved16),
        (0x8140 => pub if_power0:RW<u32>), (0x8144 => pub if_power1:RW<u32>), (0x8148 => _reserved17),
        (0x8154 => pub aux2:RW<u32>), (0x8158 => _reserved18),
        (0x8164 => pub usb2_phy:RW<u32>), (0x8168 => _reserved19),
        (0x816c => pub aux_clock:RW<u32>), (0x8170 => _reserved20),
        (0x8174 => pub latency_control:RW<u32>), (0x8178 => _reserved21),
        (0x817c => pub latency_high:RW<u32>), (0x8180 => pub latency_medium:RW<u32>), (0x8184 => pub latency_low:RW<u32>), (0x8188 => _reserved22),
        (0x818c => pub command1:RW<u32>), (0x8190 => _reserved23),
        (0x8194 => pub command3:RW<u32>), (0x8198 => _reserved24),
        (0x819c => pub throttle:RW<u32>), (0x81a0 => _reserved25),
        (0x81b4 => pub throttle2:RW<u32>), (0x81b8 => _reserved26),
        (0x81bc => pub d0i2:RW<u32>), (0x81c0 => _reserved27),
        (0x81c4 => pub usb2pm:RW<u32>), (0x81c8 => _reserved28),
        (0x8420 => pub strap2:RO<u32>), (0x8424 => _reserved28b),
        (0x84f8 => pub usb2_pdo:RW<u32,Pdo::Register>), (0x84fc => pub usb3_pdo:RW<u32,Pdo::Register>), (0x8500 => _reserved29),
        (0x8760 => pub debug:RW<u32>), (0x8764 => _reserved30),
        (0x880c => pub ssic_p1_cfg2:RW<u32>), (0x8810 => _reserved31),
        (0x883c => pub ssic_p2_cfg2:RW<u32>), (0x8840 => @END),
    }
}
impl Registers {
    pub(super) fn ss_port(
        &self,
        kind: crate::sunrise_point::dmi::Kind,
        port: usize,
    ) -> &PortRegisters {
        let base = if kind == crate::sunrise_point::dmi::Kind::H {
            16
        } else {
            12
        };
        &self.ports[base + port]
    }
}
