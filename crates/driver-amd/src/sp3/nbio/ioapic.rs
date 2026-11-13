//! The NBIO IOAPIC: features, bridge interrupt routing, ID and the plain
//! data ACPI needs (NBIO_SMU §3.6, ACPI_SMM §3.2).
//!
//! The routing registers are programmed by the generated NBIO tables; the
//! same routes are data for `_PRT` (`crate::nbio::ioapic::Route`) and
//! checked against the tables in the tests.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::Nbio;
use crate::cpu::Silicon;
use crate::nbio::ioapic::{
    BridgeRouting, IOAPIC_FEATURES_ENABLE, IOAPIC_ID, IoapicFeatures, IoapicId, ROUTES_SP3,
    ROUTES_ZP, Route, br_interrupt_routing,
};
use crate::smn::SmnAccess;

/// The bridge routes of `generation`.
pub const fn routes_for(generation: Silicon) -> &'static [Route] {
    match generation {
        Silicon::Zp => &ROUTES_ZP,
        Silicon::Ssp | Silicon::Gn => &ROUTES_SP3,
    }
}

/// Default FCH IOAPIC ID (Naples PCD 0x191 = 0x80, Rome PCD 0x26B = 0xF0).
pub const fn fch_ioapic_id(generation: Silicon) -> u8 {
    match generation {
        Silicon::Zp => 0x80,
        Silicon::Ssp | Silicon::Gn => 0xF0,
    }
}

/// Default ID of NBIO IOAPIC `n` (Naples 0x81 + n with `n` the die, Rome/
/// Milan 0xF1 + n with `n = 4 · socket + physical NBIO`, not bus order;
/// ROOTBRIDGE_2P §1). Sorting by ID then gives the donor GSI bases
/// 24/56/88/120 (socket 0) and 152/184/216/248 (socket 1), which the
/// `_PRT` windows use.
pub const fn nbio_ioapic_id(generation: Silicon, n: u8) -> u8 {
    fch_ioapic_id(generation) + 1 + n
}

/// What the MADT and the IVRS need of one NBIO IOAPIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoapicInfo {
    pub socket: u8,
    pub index: u8,
    pub id: u8,
    pub base: u64,
    /// IVRS special-device source: `bus:00.1`.
    pub bus: u8,
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// `FEATURES_ENABLE |= ID_EXT | FCH` on socket 0's FCH NBIO, plus
    /// SECONDARY everywhere else (NBIO_SMU §3.5; Naples: die 0 vs others).
    pub fn ioapic_features(&self) {
        let set = IoapicFeatures::ID_EXT::SET + IoapicFeatures::FCH::SET;
        let reg = self.reg(IOAPIC_FEATURES_ENABLE);
        // AGESA ORs 0x14 / 0x34: SECONDARY is left as is on the primary.
        if self.fch && self.socket == 0 {
            reg.modify(set);
        } else {
            reg.modify(set + IoapicFeatures::SECONDARY::SET);
        }
    }

    /// Program the bridge routes directly (the NBIO tables already do this;
    /// for flows that skip the tables).
    pub fn ioapic_routes(&self) {
        routes_for(self.generation)
            .iter()
            .enumerate()
            .for_each(|(i, r)| {
                self.reg(br_interrupt_routing(i)).modify(
                    BridgeRouting::GROUP.val(r.group as u32)
                        + BridgeRouting::SWIZZLE.val(r.swizzle as u32)
                        + BridgeRouting::MAP.val(r.map as u32),
                )
            });
    }

    /// Set the IOAPIC ID through its SMN alias (AMI NbDxe / PciRootBridge).
    pub fn set_ioapic_id(&self, id: u8) {
        self.reg(IOAPIC_ID).write(IoapicId::ID.val(id as u32));
    }

    /// The IOAPIC's current ID.
    pub fn ioapic_id(&self) -> u8 {
        self.reg(IOAPIC_ID).read(IoapicId::ID) as u8
    }

    /// ACPI data for this NBIO's IOAPIC, if its BAR is enabled.
    pub fn ioapic_info(&self) -> Option<IoapicInfo> {
        Some(IoapicInfo {
            socket: self.socket,
            index: self.index,
            id: self.ioapic_id(),
            base: self.ioapic_bar()?,
            bus: self.bus(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;

    fn route_values(
        entries: impl Iterator<Item = (crate::smn::SmnAddr, crate::sp3::nbio::table::Op)>,
    ) -> Vec<u32> {
        use crate::sp3::nbio::table::Op;
        entries
            .filter_map(|(a, op)| match op {
                Op::Rmw {
                    clear: 0x1F_0037,
                    set,
                } if (0x1430_0040..0x1430_00A0).contains(&a) => Some(set),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn routes_match_tables() {
        assert_eq!(
            route_values(
                crate::sp3::nbio::sp3::tables::rome::NBIO
                    .iter()
                    .map(|e| (e.addr, e.op))
            ),
            ROUTES_SP3.map(|r| r.value())
        );
        assert_eq!(
            route_values(
                crate::sp3::nbio::sp3::tables::milan::NBIO
                    .iter()
                    .map(|e| (e.addr, e.op))
            ),
            ROUTES_SP3.map(|r| r.value())
        );
    }

    #[test]
    fn features_and_id() {
        let (m, n0) = nbio(Silicon::Ssp, 0, 0);
        n0.ioapic_features();
        let (_, n1) = nbio(Silicon::Ssp, 1, 0x20);
        n1.ioapic_features();
        n1.set_ioapic_id(0xF2);
        let s = m.0.borrow();
        assert_eq!(s.smn[&(0, 0x1430_0000)], 0x14);
        drop(s);
        assert_eq!(n1.ioapic_id(), 0xF2);
    }
}
