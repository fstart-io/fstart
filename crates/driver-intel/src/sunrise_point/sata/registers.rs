// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PCH-H PCI, indexed SIR and non-remapping AHCI vendor fields.
use tock_registers::register_bitfields;
register_bitfields![u8,
    pub(super) Decode [
        ASSEL OFFSET(0) NUMBITS(3) [],
        MSS OFFSET(3) NUMBITS(2) [],
        INIT OFFSET(5) NUMBITS(1) []
    ],
    pub(super) Mode [ RAID OFFSET(0) NUMBITS(1) [] ],
    pub(super) RegisterLock [ LOCK OFFSET(7) NUMBITS(1) [] ],
    pub(super) CycleLow [ ENABLE OFFSET(0) NUMBITS(1) [] ]
];
register_bitfields![u16,
    pub(super) PortEnable [ PORTS OFFSET(0) NUMBITS(8) [] ],
    pub(super) RstFeatures [
        HDD_UNLOCK OFFSET(6) NUMBITS(1) [],
        LED_LOCATE OFFSET(7) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub(super) Map [
        DISABLE OFFSET(0) NUMBITS(8) [],
        SPEED_DISABLE OFFSET(16) NUMBITS(8) []
    ],
    pub(super) General [
        SCFD OFFSET(10) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(super) Pcs [ PRESENT OFFSET(16) NUMBITS(8) [] ],
    pub(super) Cycle [
        ENABLE OFFSET(0) NUMBITS(1) [],
        CLOCK_GATE OFFSET(28) NUMBITS(1) [],
        PNCAIDL OFFSET(29) NUMBITS(1) [],
        CONFIG_LOCK OFFSET(30) NUMBITS(1) [],
        ENABLE_LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(super) CycleStatus [
        PCCS OFFSET(30) NUMBITS(1) [],
        PCCD OFFSET(31) NUMBITS(1) []
    ],
    pub(super) CycleSelect [
        SELECT OFFSET(0) NUMBITS(2) [],
        CAPABILITIES OFFSET(16) NUMBITS(3) []
    ],
    pub(super) CyclePower [
        CLOCK_DATA OFFSET(16) NUMBITS(1) [],
        CLOCK_TRUNK OFFSET(17) NUMBITS(1) [],
        ICAS OFFSET(19) NUMBITS(1) [],
        TSCAS OFFSET(20) NUMBITS(1) []
    ],
    pub(super) RemapDecode [ OFFSET OFFSET(16) NUMBITS(12) [] ],
    pub(super) Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ]
];
