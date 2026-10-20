// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO IOAPIC of a die, in SMN. Its MMIO BAR is the IOHC's
//! ([`super::iohc::IOAPIC_BAR`]).
use crate::smn::SmnReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// The names are the later IOHC's; the vendor code sets 0x14 on die 0
    /// and 0x34 on the others.
    pub IoapicFeatures [
        ID_EXT OFFSET(2) NUMBITS(1) [],
        FCH OFFSET(4) NUMBITS(1) [],
        SECONDARY OFFSET(5) NUMBITS(1) []
    ]
];

pub const IOAPIC_FEATURES_ENABLE: SmnReg<IoapicFeatures::Register> = SmnReg::new(0x1430_0000);
