// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO IOAPIC of a die or IOD quadrant, in SMN. Its MMIO BAR is the
//! IOHC's ([`super::iohc::IOAPIC_BAR`]).
use crate::smn::SmnReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// The vendor code sets 0x14 on the IOAPIC with the FCH and 0x34 on
    /// the others.
    pub IoapicFeatures [
        ID_EXT OFFSET(2) NUMBITS(1) [],
        /// Legacy interrupts are forwarded to the FCH IOAPIC. ACPI `_PIC(1)`
        /// clears it on every NBIO.
        FCH OFFSET(4) NUMBITS(1) [],
        SECONDARY OFFSET(5) NUMBITS(1) []
    ],
    /// How a bridge's INTx reach the IOAPIC.
    pub BridgeRouting [
        GROUP OFFSET(0) NUMBITS(3) [],
        /// INTA..D rotation.
        SWIZZLE OFFSET(4) NUMBITS(2) [Abcd = 0, Bcda = 1, Cdab = 2, Dabc = 3],
        /// First IOAPIC pin of the group.
        MAP OFFSET(16) NUMBITS(5) []
    ],
    pub IoapicId [ID OFFSET(24) NUMBITS(8) []]
];

pub const IOAPIC_FEATURES_ENABLE: SmnReg<IoapicFeatures::Register> = SmnReg::new(0x1430_0000);
/// IOAPIC_BR_INTERRUPT_ROUTING of bridge `i`.
pub const fn br_interrupt_routing(i: usize) -> SmnReg<BridgeRouting::Register> {
    SmnReg::new(0x1430_0040 + 4 * i as u32)
}
/// The SMN alias of the IOAPIC's ID register (IOAPIC index 0).
pub const IOAPIC_ID: SmnReg<IoapicId::Register> = SmnReg::new(0x0280_1000);
