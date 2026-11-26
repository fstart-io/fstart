// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared SA PCI words: native memory_map_native.c:66–69,344–352 and
//! sa_pre_mem_native.c:1575–1622. Graphics and mapping share one GGC model.
use tock_registers::register_bitfields;

// Intel 332688-003EN pp57–66,76–81: native map word offsets/fields.
pub(crate) const GGC: u16 = 0x50;
pub(crate) const DEVEN: u16 = 0x54;
pub(crate) const DPR: u16 = 0x5c;
pub(crate) const SMRAMC: u16 = 0x88;
pub(crate) const REMAP_BASE: u16 = 0x90;
pub(crate) const REMAP_LIMIT: u16 = 0x98;
pub(crate) const TOM: u16 = 0xa0;
pub(crate) const TOUUD: u16 = 0xa8;
pub(crate) const BDSM: u16 = 0xb0;
pub(crate) const BGSM: u16 = 0xb4;
pub(crate) const TSEG: u16 = 0xb8;
pub(crate) const TOLUD: u16 = 0xbc;
pub(crate) const MESEG_BASE: u16 = 0x70;
pub(crate) const MESEG_MASK: u16 = 0x78;
// Intel 332688-003EN pp75–76. Modern SA compatible-SMRAM control,
// NOT the legacy GMCH register at0x9d. TSEG CPU protection requires SMRR.
register_bitfields![u8,
    pub(crate) SmramControl [
        C_BASE_SEG OFFSET(0) NUMBITS(3) [],
        G_SMRAME OFFSET(3) NUMBITS(1) [],
        D_LCK OFFSET(4) NUMBITS(1) [],
        D_CLS OFFSET(5) NUMBITS(1) [],
        D_OPEN OFFSET(6) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    pub(crate) GraphicsControl [
        LOCK OFFSET(0) NUMBITS(1) [],
        VGA_DISABLE OFFSET(1) NUMBITS(1) [],
        VERSATILE_ACCELERATION OFFSET(2) NUMBITS(1) [],
        GTT OFFSET(6) NUMBITS(2) [],
        STOLEN OFFSET(8) NUMBITS(8) []
    ]
];
register_bitfields![u32,
    pub(crate) DeviceEnable [ IGD OFFSET(4) NUMBITS(1) [] ],
    pub(crate) DmaProtectedRange [
        LOCK OFFSET(0) NUMBITS(1) [],
        STATUS OFFSET(1) NUMBITS(1) [],
        ENABLE OFFSET(2) NUMBITS(1) [],
        SIZE_MIB OFFSET(4) NUMBITS(8) [],
        TOP_MIB OFFSET(20) NUMBITS(12) []
    ],
    pub(crate) LowMemoryBoundary [
        LOCK OFFSET(0) NUMBITS(1) [],
        ADDRESS_MIB OFFSET(20) NUMBITS(12) []
    ]
];
register_bitfields![u64,
    pub(crate) MemoryBoundary [
        LOCK OFFSET(0) NUMBITS(1) [],
        ADDRESS_MIB OFFSET(20) NUMBITS(19) []
    ],
    pub(crate) MeSegmentBase [ ADDRESS_MIB OFFSET(20) NUMBITS(19) [] ],
    pub(crate) MeSegmentMask [
        LOCK OFFSET(10) NUMBITS(1) [],
        STOLEN_ENABLE OFFSET(11) NUMBITS(1) [],
        MASK_MIB OFFSET(20) NUMBITS(19) []
    ]
];
