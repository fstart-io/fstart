// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF4 I/O/VGA routing: openSIL SilFabricRegistersDfX.h F0xD00/D04
//! (eight pairs) and F0xC08. DF2/3's offsets AND base-field shift differ.
use super::super::DfReg;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    pub Base [READ OFFSET(0) NUMBITS(1) [], WRITE OFFSET(1) NUMBITS(1) [], ISA OFFSET(5) NUMBITS(1) [], ADDRESS OFFSET(16) NUMBITS(13) []],
    pub Limit [DESTINATION OFFSET(0) NUMBITS(12) [], ADDRESS OFFSET(16) NUMBITS(13) []],
    pub Vga [ENABLE OFFSET(0) NUMBITS(1) [], NONPOSTED OFFSET(1) NUMBITS(1) [], CPU_DISABLE OFFSET(2) NUMBITS(1) [], DESTINATION OFFSET(4) NUMBITS(12) []]
];
pub const PAIRS: u8 = 8;
pub const BASE: DfReg<Base::Register> = DfReg::new(0, 0xd00);
pub const LIMIT: DfReg<Limit::Register> = DfReg::new(0, 0xd04);
pub const VGA: DfReg<Vga::Register> = DfReg::new(0, 0xc08);
pub const STRIDE: u16 = 8;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    pub base: u32,
    pub limit: u32,
}
pub type Table = [Raw; PAIRS as usize];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub start: u32,
    pub end: u32,
    pub destination: u16,
    pub isa: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    Unavailable,
    InvalidTable,
    UnsupportedDefault,
    InvalidPlan,
    Stale,
    Readback,
}
impl Route {
    pub fn decode(raw: Raw) -> Result<Option<Self>, Error> {
        if raw.base == u32::MAX || raw.limit == u32::MAX {
            return Err(Error::Unavailable);
        }
        let base = LocalRegisterCopy::<u32, Base::Register>::new(raw.base);
        match (base.is_set(Base::READ), base.is_set(Base::WRITE)) {
            (false, false) => return Ok(None),
            (true, true) => {}
            _ => return Err(Error::InvalidTable),
        }
        let limit = LocalRegisterCopy::<u32, Limit::Register>::new(raw.limit);
        let start = base.read(Base::ADDRESS) << 12;
        let end = (limit.read(Limit::ADDRESS) + 1) << 12;
        if start >= end {
            return Err(Error::InvalidTable);
        }
        Ok(Some(Self {
            start,
            end,
            destination: limit.read(Limit::DESTINATION) as u16,
            isa: base.is_set(Base::ISA),
        }))
    }
}
pub fn snapshot<P: fstart_pci::ConfigRegionAccess>(df: &super::Df<P>) -> Result<Table, Error> {
    let mut table = [Raw::default(); PAIRS as usize];
    for pair in 0..PAIRS {
        let raw = Raw {
            base: df.reg(BASE.at(pair, STRIDE)).get(),
            limit: df.reg(LIMIT.at(pair, STRIDE)).get(),
        };
        Route::decode(raw)?;
        table[pair as usize] = raw;
    }
    Ok(table)
}
/// Install into an empty reset table, or replace one known primary-only
/// reset route. Never arbitrary live range surgery: preserve its high tail.
pub struct Plan {
    before: Table,
    after: Table,
}
impl Plan {
    pub fn prepare(before: &Table, primary: u16, routes: &[Route]) -> Result<Self, Error> {
        let mut reset = None;
        for raw in before {
            if let Some(route) = Route::decode(*raw)?
                && reset.replace(route).is_some()
            {
                return Err(Error::UnsupportedDefault);
            }
        }
        let reset = Some(reset.unwrap_or(Route {
            start: 0,
            end: 0x10000,
            destination: primary,
            isa: false,
        }))
        .filter(|r| r.start == 0 && r.end >= 0x10000 && r.destination == primary && !r.isa)
        .ok_or(Error::UnsupportedDefault)?;
        if routes.is_empty() || routes.len() + usize::from(reset.end > 0x10000) > PAIRS as usize {
            return Err(Error::InvalidPlan);
        }
        let mut next = 0;
        for route in routes {
            if route.start != next
                || route.end <= route.start
                || route.end > 0x10000
                || route.start & 0xfff != 0
                || route.end & 0xfff != 0
                || route.destination > 0xfff
                || route.isa
            {
                return Err(Error::InvalidPlan);
            }
            next = route.end;
        }
        if next != 0x10000 || routes[0].destination != primary || routes[0].end < 0x1000 {
            return Err(Error::InvalidPlan);
        }
        let tail = Route {
            start: 0x10000,
            ..reset
        };
        let mut after = *before;
        for raw in &mut after {
            let mut base = LocalRegisterCopy::<u32, Base::Register>::new(raw.base);
            base.modify(Base::READ::CLEAR + Base::WRITE::CLEAR);
            raw.base = base.get();
        }
        for (index, route) in routes
            .iter()
            .chain((reset.end > 0x10000).then_some(&tail))
            .enumerate()
        {
            let mut base = LocalRegisterCopy::<u32, Base::Register>::new(after[index].base);
            let mut limit = LocalRegisterCopy::<u32, Limit::Register>::new(after[index].limit);
            base.modify(
                Base::READ::SET
                    + Base::WRITE::SET
                    + Base::ISA::CLEAR
                    + Base::ADDRESS.val(route.start >> 12),
            );
            limit.modify(
                Limit::DESTINATION.val(route.destination.into())
                    + Limit::ADDRESS.val((route.end - 1) >> 12),
            );
            after[index] = Raw {
                base: base.get(),
                limit: limit.get(),
            };
        }
        Ok(Self {
            before: *before,
            after,
        })
    }
    pub fn table(&self) -> &Table {
        &self.after
    }
    /// # Safety
    /// BSP-exclusive DF4 with all CPUs/SMI/firmware I/O consumers quiescent.
    /// No port-I/O instruction may execute while routing is disabled. Routes
    /// preserve every fixed FCH port and describe caller-owned PCI I/O pools.
    /// Any failure requires fail-stop/reset, never a retry or partial handoff.
    pub unsafe fn install<P: fstart_pci::ConfigRegionAccess>(
        self,
        df: &super::Df<P>,
    ) -> Result<(), Error> {
        if snapshot(df)? != self.before {
            return Err(Error::Stale);
        }
        // All configuration uses ECAM/FICAA3 MMIO, not port I/O. Disable
        // before changing any overlapping old/new range; publish enables last.
        for pair in 0..PAIRS {
            df.reg(BASE.at(pair, STRIDE))
                .modify(Base::READ::CLEAR + Base::WRITE::CLEAR);
        }
        for pair in 0..PAIRS {
            let mut disabled =
                LocalRegisterCopy::<u32, Base::Register>::new(self.after[pair as usize].base);
            disabled.modify(Base::READ::CLEAR + Base::WRITE::CLEAR);
            df.reg(LIMIT.at(pair, STRIDE))
                .set(self.after[pair as usize].limit);
            df.reg(BASE.at(pair, STRIDE)).set(disabled.get());
        }
        for pair in 0..PAIRS {
            df.reg(BASE.at(pair, STRIDE))
                .set(self.after[pair as usize].base);
        }
        if snapshot(df)? != self.after {
            return Err(Error::Readback);
        }
        Ok(())
    }
}
/// # Safety
/// Exclusive DF4, checked display IOHC destination and reserved VGA legacy
/// memory/ports. Do not displace another enabled display destination.
pub unsafe fn route_vga<P: fstart_pci::ConfigRegionAccess>(
    df: &super::Df<P>,
    destination: u16,
) -> Result<(), Error> {
    let mut before = df.reg(VGA).extract();
    if before.get() == u32::MAX || destination > 0xfff {
        return Err(Error::Unavailable);
    }
    if before.is_set(Vga::ENABLE) && before.read(Vga::DESTINATION) != u32::from(destination) {
        return Err(Error::UnsupportedDefault);
    }
    before.modify(
        Vga::ENABLE::SET + Vga::CPU_DISABLE::CLEAR + Vga::DESTINATION.val(destination.into()),
    );
    df.reg(VGA).set(before.get());
    if df.reg(VGA).get() != before.get() {
        return Err(Error::Readback);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(start: u32, end: u32, destination: u16) -> Raw {
        Raw {
            base: (Base::READ::SET + Base::WRITE::SET + Base::ADDRESS.val(start >> 12)).value,
            limit: (Limit::DESTINATION.val(destination.into())
                + Limit::ADDRESS.val((end - 1) >> 12))
            .value,
        }
    }
    fn routes() -> [Route; 4] {
        core::array::from_fn(|i| Route {
            start: i as u32 * 0x4000,
            end: (i as u32 + 1) * 0x4000,
            destination: [0x22, 0x20, 0x21, 0x23][i],
            isa: false,
        })
    }
    #[test]
    fn df4_offsets_and_shift_are_not_df3() {
        assert_eq!(BASE.register(), 0xd00);
        assert_eq!(LIMIT.register(), 0xd04);
        assert_eq!(VGA.register(), 0xc08);
        assert_eq!(
            raw(0xa000, 0x10000, 0x20),
            Raw {
                base: 0xa0003,
                limit: 0xf0020
            }
        );
    }
    #[test]
    fn partitions_reset_route_preserves_tail_and_reserved_bits() {
        let mut before = [Raw {
            base: 0x80000000,
            limit: 0x40000000,
        }; PAIRS as usize];
        before[7] = raw(0, 0x2000000, 0x22);
        let plan = Plan::prepare(&before, 0x22, &routes()).unwrap();
        for (i, wanted) in routes().iter().enumerate() {
            assert_eq!(Route::decode(plan.table()[i]), Ok(Some(*wanted)));
            assert_eq!(plan.table()[i].base & 0x80000000, 0x80000000);
        }
        assert_eq!(
            Route::decode(plan.table()[4]).unwrap().unwrap().end,
            0x2000000
        );
        assert_eq!(Route::decode(plan.table()[7]), Ok(None));
    }
    #[test]
    fn observed_empty_genoa_reset_installs_only_four_port_ranges() {
        let before = [Raw {
            base: 0,
            limit: 0x20,
        }; PAIRS as usize];
        let plan = Plan::prepare(&before, 0x22, &routes()).unwrap();
        for (index, route) in routes().into_iter().enumerate() {
            assert_eq!(Route::decode(plan.table()[index]), Ok(Some(route)));
        }
        for raw in &plan.table()[4..] {
            assert_eq!(Route::decode(*raw), Ok(None));
        }
        assert_eq!(
            before,
            [Raw {
                base: 0,
                limit: 0x20
            }; PAIRS as usize]
        );
    }
    #[test]
    fn rejects_unknown_live_ownership_gaps_partial_and_isa_modes() {
        let mut before = [Raw::default(); PAIRS as usize];
        assert!(Plan::prepare(&before, 0x22, &routes()).is_ok());
        before[0] = raw(0, 0x10000, 0x22);
        assert!(Plan::prepare(&before, 0x22, &routes()).is_ok());
        let mut gap = routes();
        gap[1].start += 0x1000;
        assert!(Plan::prepare(&before, 0x22, &gap).is_err());
        before[1] = raw(0x10000, 0x20000, 0x20);
        assert!(Plan::prepare(&before, 0x22, &routes()).is_err());
        before[1] = Raw::default();
        before[0].base |= Base::ISA::SET.value;
        assert!(Plan::prepare(&before, 0x22, &routes()).is_err());
        before[0].base = Base::READ::SET.value;
        assert_eq!(Route::decode(before[0]), Err(Error::InvalidTable));
    }
}
