// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian (`Ql`), the FCH of the WH dual-IOD platform (up to 4 FCH instances:
//! 2 sockets × 2 IODs).
//!
//! Primary FCH takeover, ACPI handoff, and GPIO PERST release reuse
//! [`super::songshan`] (`Pin(26)` at drive strength 2 writes `0x00C4_0000` to
//! `acpi_mmio + 0x1568`, matching `FchIp2IpDeassertPciePerstQl`). This module
//! adds Qilian Multi-FCH secondary ACPI MMIO relocation (`SMN 0x02D0_13D4`)
//! and secondary AB FabricID offsets.

use tock_registers::interfaces::Writeable;
use tock_registers::register_bitfields;

use super::gpio::Pin;
pub use super::songshan::{Perst, acpi_mode, release_perst, route_legacy, take_over};
use crate::smn::{SmnAccess, SmnReg};

/// Maximum FCH instances in a 2P dual-IOD system.
pub const MAX_FCHS: u8 = 4;

/// Common PCIe PERST# output (`acpi_mmio + 0x1568 = 0x00C4_0000`).
pub const PERST_COMMON: Perst = Perst {
    pin: Pin(26),
    drive_strength: 2,
};

register_bitfields![u32,
    /// SMN 0x02D0_13D4 (32-bit): Qilian secondary FCH ACPI MMIO base/enable
    /// (`(base & 0xFFFF_0000) | 0x100`).
    pub AcpiMmioDecode [
        ENABLE OFFSET(8) NUMBITS(1) [],
        BASE OFFSET(16) NUMBITS(16) []
    ]
];

/// SMN 0x02D0_13D4 (32-bit): secondary FCH ACPI MMIO decode register.
pub const SECONDARY_ACPI_MMIO: SmnReg<AcpiMmioDecode::Register> = SmnReg::new(0x02d0_13d4);

/// Relocate a secondary Qilian FCH's ACPI MMIO window to 64 KiB-aligned `base`.
/// Returns `false` if `base` is not 64 KiB aligned.
pub fn set_secondary_acpi_mmio(smn: &impl SmnAccess, base: u32) -> bool {
    if (base & 0xffff) != 0 {
        return false;
    }
    smn.reg(SECONDARY_ACPI_MMIO)
        .write(AcpiMmioDecode::BASE.val(base >> 16) + AcpiMmioDecode::ENABLE::SET);
    true
}

/// Secondary FCH AB routing FabricID offset for FCH instance `fch_index` (`0..4`).
/// Primary FCH (`0`) has offset `0`; secondaries `1..=3` use `0x40`, `0x80`, `0xC0`.
pub const fn ab_fabric_id_offset(fch_index: u8) -> Option<u16> {
    if fch_index >= MAX_FCHS {
        return None;
    }
    Some((fch_index as u16) * 0x40)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;
    use crate::smn::testing::SmnSpace;

    #[test]
    fn secondary_acpi_mmio_encodes_base_and_enable_bit8() {
        let space = SmnSpace::default();
        assert!(!set_secondary_acpi_mmio(&space.smn(), 0xfed8_1000));
        assert!(set_secondary_acpi_mmio(&space.smn(), 0xfed9_0000));
        assert_eq!(space.get(SECONDARY_ACPI_MMIO.address()), 0xfed9_0100);
    }

    #[test]
    fn ab_fabric_offsets_and_common_perst_match_ql() {
        assert_eq!(ab_fabric_id_offset(0), Some(0x00));
        assert_eq!(ab_fabric_id_offset(1), Some(0x40));
        assert_eq!(ab_fabric_id_offset(2), Some(0x80));
        assert_eq!(ab_fabric_id_offset(3), Some(0xc0));
        assert_eq!(ab_fabric_id_offset(4), None);

        let hw = fch();
        assert!(release_perst(&hw, &[PERST_COMMON]));
        assert_eq!(hw.acpi.get(0x1568, 4), 0x00c4_0000);
    }
}
