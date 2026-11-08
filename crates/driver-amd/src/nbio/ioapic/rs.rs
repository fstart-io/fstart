// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs IOD NBIO IOAPIC ownership, not SP3's route table or a BAR-address replay.
//! openSIL NbioIoApic.c / NbioIoapicTbl.h: 24 routes, independently relocated
//! per physical IOS; the cold BAR is disabled. Genoa B0 programs all four
//! roots so; Siena A2's vendor firmware programs the same routes, IDs
//! (F1h + IOS) and GSI bases on its three.
use super::{
    BridgeRouting, IOAPIC_FEATURES_ENABLE, IOAPIC_ID, IoapicFeatures, IoapicId, Route,
    br_interrupt_routing,
};
use crate::{
    nbio::iohc,
    smn::{SmnAccess, SmnReg},
};
tock_registers::register_bitfields![u32,
    /// openSIL NbioNbifTbl endpoint-function enables and bridge enables.
    FunctionInterrupts [
        FUNCTIONS OFFSET(0) NUMBITS(8) [],
        BRIDGES OFFSET(8) NUMBITS(2) []
    ]
];
fn nbif_interrupts(index: usize) -> SmnReg<FunctionInterrupts::Register> {
    // NBIF tables run only on EVEN IOS (NbioBaseInitGenoa.c). These four
    // absolute blocks are shared with the odd root, not eight local copies.
    SmnReg::new(0x1013a008 + ((index as u32) << 20))
}
/// First NBIF: functions 0/1/4/5 plus both bridge enables; second:
/// functions 0/1 only (openSIL NbioNbifTbl; NFX NbioBaseConfiguration's NBIF
/// table writes the same values, with F2 disabled).
fn nbif_value(
    index: usize,
) -> tock_registers::fields::FieldValue<u32, FunctionInterrupts::Register> {
    if index.is_multiple_of(2) {
        FunctionInterrupts::FUNCTIONS.val(0x33) + FunctionInterrupts::BRIDGES.val(3)
    } else {
        FunctionInterrupts::FUNCTIONS.val(3) + FunctionInterrupts::BRIDGES.val(0)
    }
}
use tock_registers::interfaces::{Readable, Writeable};
/// Complete openSIL Genoa recommended table, not the 22-entry SP3 table.
pub const ROUTES: [Route; 24] = {
    let triples = [
        (0, 0, 0),
        (1, 0, 0),
        (2, 0, 0),
        (3, 0, 0),
        (4, 0, 0),
        (4, 2, 0),
        (3, 2, 0),
        (2, 2, 0),
        (1, 2, 0),
        (0, 2, 1),
        (0, 1, 1),
        (1, 1, 1),
        (2, 1, 1),
        (3, 1, 1),
        (4, 1, 1),
        (4, 3, 1),
        (3, 3, 1),
        (2, 3, 1),
        (1, 3, 2),
        (0, 3, 2),
        (0, 0, 2),
        (1, 0, 2),
        (2, 0, 3),
        (3, 0, 3),
    ];
    let mut routes = [Route {
        group: 0,
        swizzle: 0,
        map: 0,
    }; 24];
    let mut i = 0;
    while i < 24 {
        routes[i] = Route {
            group: triples[i].0,
            swizzle: triples[i].1,
            map: triples[i].2,
        };
        i += 1;
    }
    routes
};
pub const WINDOW_SIZE: u64 = 0x10000;
pub const PINS: u32 = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub physical: u8,
    pub base: u32,
    pub id: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bridge {
    pub slot: u8,
    pub devfn: u8,
    pub interrupt_pin: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    Identity,
    ColdState,
    Stale,
    Readback,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    features: u32,
    id: u32,
    low: u32,
    high: u32,
    routes: [u32; 24],
}
fn snapshot(smn: &impl SmnAccess, physical: u8) -> Snapshot {
    let offset = u32::from(physical) << 20;
    Snapshot {
        features: smn.reg(IOAPIC_FEATURES_ENABLE.offset(offset)).get(),
        id: smn.reg(IOAPIC_ID.offset(offset)).get(),
        low: smn.reg(iohc::IOAPIC_BAR.low.offset(offset)).get(),
        high: smn.reg(iohc::IOAPIC_BAR.high.offset(offset)).get(),
        routes: core::array::from_fn(|i| smn.reg(br_interrupt_routing(i).offset(offset)).get()),
    }
}
pub struct Plan {
    targets: heapless::Vec<Target, 4>,
    before: heapless::Vec<Snapshot, 4>,
}
impl Plan {
    /// No writes. Caller proves IOD-global SMN relocation against PCI IOMMU
    /// identities before using it, and supplies one disjoint UC/DF-routed
    /// window per live root.
    pub fn prepare(
        smn: &impl SmnAccess,
        signature: u32,
        targets: &[Target],
    ) -> Result<Self, Error> {
        // Genoa B0 and Siena A2, the measured steppings.
        if !matches!(signature, 0x00a1_0f10 | 0x00aa_0f02) || targets.is_empty() {
            return Err(Error::Identity);
        }
        let targets: heapless::Vec<Target, 4> =
            heapless::Vec::from_slice(targets).map_err(|_| Error::Identity)?;
        for (i, t) in targets.iter().enumerate() {
            if t.physical >= 4
                || t.id == 0
                || t.base < 0x80000000
                || t.base >= 0xe0000000
                || u64::from(t.base) % WINDOW_SIZE != 0
                || targets[..i]
                    .iter()
                    .any(|p| p.physical == t.physical || p.id == t.id || p.base == t.base)
            {
                return Err(Error::Identity);
            }
        }
        let before: heapless::Vec<Snapshot, 4> =
            targets.iter().map(|t| snapshot(smn, t.physical)).collect();
        for state in &before {
            if state.features == u32::MAX
                || state.features
                    & (IoapicFeatures::ID_EXT::SET.mask() | IoapicFeatures::FCH::SET.mask())
                    != IoapicFeatures::ID_EXT::SET.value
                || state.id != 0
                || state.low != 0xfec00000
                || state.high != 0
                || (state.routes != [0; 24] && state.routes != ROUTES.map(Route::value))
            {
                return Err(Error::ColdState);
            }
        }
        // Cold, or already the recommended state (the vendor NBIO base
        // table programs the same routes and NBIF interrupt enables).
        if targets.iter().any(|t| {
            let i = usize::from(t.physical);
            let current = smn.reg(nbif_interrupts(i)).get();
            current != 0 && current != nbif_value(i).value
        }) {
            return Err(Error::ColdState);
        }
        Ok(Self { targets, before })
    }
    /// Enable only after every target and DF reservation is preflighted.
    /// # Safety
    /// Exclusive BSP SMN; checked Genoa B0/global relocation, IF=0 on all
    /// CPUs, SMI disabled, no active legacy consumer, devices DMA-disabled.
    /// Every base is UC/identity mapped and routed to this exact IOHC by DF.
    /// Any failure is fail-stop until reset; there is no retry/rollback.
    pub unsafe fn install(
        self,
        smn: &impl SmnAccess,
        mut mask: impl FnMut(Target) -> Result<(), Error>,
    ) -> Result<(), Error> {
        for (i, t) in self.targets.iter().enumerate() {
            if snapshot(smn, t.physical) != self.before[i] {
                return Err(Error::Stale);
            }
        }
        for (i, t) in self.targets.iter().enumerate() {
            let offset = u32::from(t.physical) << 20;
            for (index, route) in ROUTES.iter().enumerate() {
                smn.reg(br_interrupt_routing(index).offset(offset)).write(
                    BridgeRouting::GROUP.val(route.group.into())
                        + BridgeRouting::SWIZZLE.val(route.swizzle.into())
                        + BridgeRouting::MAP.val(route.map.into()),
                );
            }
            smn.reg(IOAPIC_ID.offset(offset))
                .write(IoapicId::ID.val(t.id.into()));
            iohc::IOAPIC_BAR.map_registers(
                &smn.reg(iohc::IOAPIC_BAR.low.offset(offset)),
                &smn.reg(iohc::IOAPIC_BAR.high.offset(offset)),
                t.base,
            );
            for (previous_index, previous) in self.targets[..=i].iter().enumerate() {
                let got = snapshot(smn, previous.physical);
                if got.id != u32::from(previous.id) << 24
                    || got.low != (previous.base | 1)
                    || got.high != 0
                    || got.routes != ROUTES.map(Route::value)
                    || got.features != self.before[previous_index].features
                {
                    return Err(Error::Readback);
                }
            }
            // Verify MMIO identity/version and mask all RTEs before enabling
            // endpoint INTx. Any error retains all ownership through reset.
            mask(*t)?;
            // All not-yet-owned instances must still be exactly cold.
            for (j, later) in self.targets.iter().enumerate().skip(i + 1) {
                if snapshot(smn, later.physical) != self.before[j] {
                    return Err(Error::Readback);
                }
            }
        }
        for index in self.targets.iter().map(|t| usize::from(t.physical)) {
            let value = nbif_value(index);
            match smn.reg(nbif_interrupts(index)).get() {
                0 => {}
                // Already enabled by the vendor NBIO base table: same value.
                current if current == value.value => continue,
                _ => return Err(Error::Stale),
            }
            smn.reg(nbif_interrupts(index)).write(value);
            if smn.reg(nbif_interrupts(index)).get() != value.value {
                return Err(Error::Readback);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;
    fn targets() -> [Target; 4] {
        core::array::from_fn(|i| Target {
            physical: i as u8,
            base: 0xd8400000 + i as u32 * 0x10000,
            id: 0xf1 + i as u8,
        })
    }
    fn model() -> SmnSpace {
        let s = SmnSpace::default();
        for i in 0..4 {
            let o = i << 20;
            s.set(IOAPIC_FEATURES_ENABLE.offset(o), 0x204);
            s.set(iohc::IOAPIC_BAR.low.offset(o), 0xfec00000);
        }
        s
    }
    #[test]
    fn independent_bars_routes_ids_and_unknown_feature_bits_survive() {
        let s = model();
        let t = targets();
        let p = Plan::prepare(&s.smn(), 0xa10f10, &t).unwrap();
        unsafe { p.install(&s.smn(), |_| Ok(())) }.unwrap();
        for t in t {
            let g = snapshot(&s.smn(), t.physical);
            assert_eq!(g.low, t.base | 1);
            assert_eq!(g.id, u32::from(t.id) << 24);
            assert_eq!(g.features, 0x204);
            assert_eq!(g.routes, ROUTES.map(Route::value));
        }
        assert_eq!(
            ROUTES[22],
            Route {
                group: 2,
                swizzle: 0,
                map: 3
            }
        );
        assert_eq!(ROUTES[19].secondary_pin(0), 1);
        assert_eq!(ROUTES[22].secondary_pin(2), 10);
        for i in 0..4 {
            assert_eq!(
                s.get(nbif_interrupts(i)),
                if i % 2 == 0 { 0x333 } else { 3 }
            );
        }
    }
    #[test]
    fn siena_owns_its_three_roots_and_never_touches_the_fused_one() {
        let s = model();
        s.set(nbif_interrupts(1), u32::MAX); // IOS1 fused off: no NBIF block.
        let t = [targets()[0], targets()[2], targets()[3]];
        let p = Plan::prepare(&s.smn(), 0xaa0f02, &t).unwrap();
        unsafe { p.install(&s.smn(), |_| Ok(())) }.unwrap();
        assert_eq!(s.get(nbif_interrupts(0)), 0x333);
        assert_eq!(s.get(nbif_interrupts(2)), 0x333);
        assert_eq!(s.get(nbif_interrupts(3)), 3);
        assert_eq!(s.get(nbif_interrupts(1)), u32::MAX);
        assert_eq!(snapshot(&s.smn(), 1).low, 0xfec00000);
        assert!(Plan::prepare(&s.smn(), 0xaa0f01, &t).is_err());
    }
    #[test]
    fn the_vendor_base_table_state_is_accepted_but_nothing_else() {
        // NbioBaseConfiguration already wrote the same routes and NBIF
        // interrupt enables; the plan still relocates and verifies.
        let s = model();
        for i in 0..4 {
            for (index, route) in ROUTES.iter().enumerate() {
                s.set(br_interrupt_routing(index).offset(i << 20), route.value());
            }
            s.set(nbif_interrupts(i as usize), nbif_value(i as usize).value);
        }
        let p = Plan::prepare(&s.smn(), 0xa10f10, &targets()).unwrap();
        unsafe { p.install(&s.smn(), |_| Ok(())) }.unwrap();
        assert_eq!(s.get(nbif_interrupts(0)), 0x333);
        s.set(nbif_interrupts(1), 0x7);
        assert_eq!(
            Plan::prepare(&s.smn(), 0xa10f10, &targets()).err(),
            Some(Error::ColdState)
        );
        let s = model();
        s.set(br_interrupt_routing(5).offset(2 << 20), 0x24);
        assert_eq!(
            Plan::prepare(&s.smn(), 0xa10f10, &targets()).err(),
            Some(Error::ColdState)
        );
    }
    #[test]
    fn masking_failure_never_enables_nbif_or_touches_later_ioapics() {
        let s = model();
        let p = Plan::prepare(&s.smn(), 0xa10f10, &targets()).unwrap();
        assert_eq!(
            unsafe { p.install(&s.smn(), |_| Err(Error::Readback)) },
            Err(Error::Readback)
        );
        for i in 0..4 {
            assert_eq!(s.get(nbif_interrupts(i)), 0);
        }
        assert_eq!(snapshot(&s.smn(), 1).low, 0xfec00000);
        assert_eq!(snapshot(&s.smn(), 1).routes, [0; 24]);
    }
    #[test]
    fn cold_all_target_and_stale_preflight_fail_closed() {
        let s = model();
        let mut t = targets();
        t[3].base = t[0].base;
        assert!(Plan::prepare(&s.smn(), 0xa10f10, &t).is_err());
        assert!(Plan::prepare(&s.smn(), 0xa10f11, &targets()).is_err());
        s.set(iohc::IOAPIC_BAR.low.offset(3 << 20), 0xfec00001);
        assert!(Plan::prepare(&s.smn(), 0xa10f10, &targets()).is_err());
        assert_eq!(snapshot(&s.smn(), 0).routes, [0; 24]);
        let s = model();
        let p = Plan::prepare(&s.smn(), 0xa10f10, &targets()).unwrap();
        s.set(IOAPIC_ID.offset(3 << 20), 1);
        assert_eq!(
            unsafe { p.install(&s.smn(), |_| Ok(())) },
            Err(Error::Stale)
        );
        assert_eq!(snapshot(&s.smn(), 0).routes, [0; 24]);
    }
}
