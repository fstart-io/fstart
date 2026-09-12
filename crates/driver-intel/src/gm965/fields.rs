//! GM965 egress/DMI register field definitions.
//!
//! `register_bitfields!` catalog for the PCIe virtual-channel and link
//! read-modify-write sites in [`super`](crate::gm965). Shapes mirror the
//! PCIe-spec VC layouts also used by the i945 driver; each driver keeps its
//! own copy so chipset bit truth stays local. Stepping-tuned magic,
//! whole-word constants, and 8-bit VC0 accesses stay numeric at the use
//! site with comments.

use tock_registers::register_bitfields;

register_bitfields![u32,
    /// Egress VC1 resource capability (EPBAR EPVC1RCAP).
    pub EPVC1RCAP_REG [
        VC1_MTS OFFSET(16) NUMBITS(7) []
    ],

    /// Egress/DMI VC1 control (EPVC1RCTL / DMIVC1RCTL).
    pub VC1RCTL_REG [
        VC_EN OFFSET(31) NUMBITS(1) [],
        VC_ID OFFSET(24) NUMBITS(3) [],
        ARB_LOAD OFFSET(16) NUMBITS(1) []
    ],

    /// DMI VC misc (DMIBAR 0x200): root-port topology.
    pub DMI200_REG [
        TOPO OFFSET(26) NUMBITS(2) []
    ],

    /// DMI link capabilities (DMIBAR DMILCAP).
    pub DMILCAP_REG [
        L1_EXIT_LAT OFFSET(15) NUMBITS(3) [],
        L0S_EXIT_LAT OFFSET(12) NUMBITS(3) []
    ]
];

register_bitfields![u8,
    /// DMI link control (DMIBAR DMILCTL): ASPM enable.
    pub DMILCTL_REG [
        ASPM_CTRL OFFSET(0) NUMBITS(2) []
    ]
];
