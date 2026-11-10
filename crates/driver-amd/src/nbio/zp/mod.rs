// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO register tables of the ZP die (vendor AmdNbioBaseZPPei), which
//! Zeppelin (AM4, SP4, SP4r2) and Naples (SP3) are both built from. The
//! PEIMs run the same tables; what differs is the property word that
//! selects the conditional records, and one more record on Naples
//! ([`tables::BASE_NAPLES`]).
use crate::smn::{self, Reg, SmnAddr};
use crate::table::Op;

#[rustfmt::skip]
pub mod tables;

/// Property bits of the tables (sp4r2 NBIO_BASE_ZP §3.2).
pub mod property {
    /// IOMMU disabled (PCD 0x02 == 0).
    pub const IOMMU_OFF: u32 = 0x2;
    /// SST clock gating disabled (PCD 0x8B == 0).
    pub const SST_CG_OFF: u32 = 0x4;
    /// IOMMU L1 clock gating (PCD 0x86).
    pub const IOMMU_L1_CG: u32 = 0x40;
    /// IOMMU L2 clock gating (PCD 0x87).
    pub const IOMMU_L2_CG: u32 = 0x80;
    /// Package in {SP4, SP4r2, SP3}.
    pub const SERVER_PACKAGE: u32 = 0x800;
    /// IOHC clock gating (PCD 0x88).
    pub const IOHC_CG: u32 = 0x2000_0000;
    /// RAS / poison (PCD 0x14).
    pub const RAS: u32 = 0x4000_0000;
}

/// A record of the ZP tables ("legacy" GNB bytecode).
///
/// A record with a non-zero `property` applies when it shares a bit with
/// the live [`property`] word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyEntry {
    pub addr: SmnAddr,
    pub op: Op<SmnAddr, u32>,
    pub property: u32,
}

impl LegacyEntry {
    pub const fn write(addr: SmnAddr, value: u32) -> Self {
        Self {
            addr,
            op: Op::Write(value),
            property: 0,
        }
    }

    pub const fn rmw(addr: SmnAddr, clear: u32, set: u32) -> Self {
        Self {
            addr,
            op: Op::Update { clear, set },
            property: 0,
        }
    }

    pub const fn copy(to: SmnAddr, from: SmnAddr) -> Self {
        Self {
            addr: to,
            op: Op::Copy { from },
            property: 0,
        }
    }

    pub const fn property(self, property: u32) -> Self {
        Self { property, ..self }
    }

    /// Whether the record applies under the live `property`.
    pub const fn applies(&self, property: u32) -> bool {
        self.property == 0 || self.property & property != 0
    }

    /// The record for [`crate::smn::apply`], without its property.
    pub const fn record(&self) -> Reg {
        match self.op {
            Op::Write(value) => smn::write(self.addr, value),
            Op::Update { clear, set } => smn::rmw(self.addr, clear, set),
            Op::Copy { from } => smn::copy(self.addr, from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without the conditional records the tables keep their order.
    #[test]
    fn property_selects_the_conditional_records() {
        let all = property::IOMMU_OFF
            | property::SST_CG_OFF
            | property::IOMMU_L1_CG
            | property::IOMMU_L2_CG
            | property::SERVER_PACKAGE
            | property::IOHC_CG
            | property::RAS;
        let count = |table: &[LegacyEntry], live| table.iter().filter(|e| e.applies(live)).count();
        assert_eq!((tables::BASE.len(), tables::IOMMU.len()), (158, 38));
        assert_eq!(count(tables::BASE, all), 158);
        assert_eq!(count(tables::BASE, 0), 151);
        assert_eq!(count(tables::IOMMU, 0), 30);
        assert_eq!(tables::BASE_NAPLES.len(), 1);
    }

    #[test]
    fn records_convert_with_their_operation() {
        let record = LegacyEntry::rmw(0x100, 0xff, 0x12).property(0x40).record();
        assert_eq!(record.reg.address(), 0x100);
        assert!(matches!(
            record.op,
            Op::Update {
                clear: 0xff,
                set: 0x12
            }
        ));
        assert!(matches!(
            LegacyEntry::write(0x104, 7).record().op,
            Op::Write(7)
        ));
        assert!(matches!(
            LegacyEntry::copy(0x108, 0x10c).record().op,
            Op::Copy { from } if from.address() == 0x10c
        ));
    }
}
