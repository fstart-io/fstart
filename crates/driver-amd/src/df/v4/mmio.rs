// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF4 MMIO routing, distinct from DF2/3's F0x200 layout. Register geometry
//! and control fields: public openSIL DfX; independently instruction-checked
//! Siena rs.map.mmio_writer (AmdSocSp5RsPei), also NFX ffa6d46d's
//! instructions ffa6d4cd..ffa6d520. This module invents no policy.
use super::super::DfReg;
use tock_registers::{LocalRegisterCopy, register_bitfields};
register_bitfields![u32,
    pub Control [READ OFFSET(0) NUMBITS(1) [],WRITE OFFSET(1) NUMBITS(1) [],CPU_DISABLE OFFSET(2) NUMBITS(1) [],NONPOSTED OFFSET(3) NUMBITS(1) [],DESTINATION OFFSET(16) NUMBITS(12) []],
    Extension [BASE_HIGH OFFSET(0) NUMBITS(8) [],LIMIT_HIGH OFFSET(16) NUMBITS(8) []]
];
pub const PAIRS: u8 = 16;
pub const BASE: DfReg = DfReg::new(0, 0xd80);
pub const LIMIT: DfReg = DfReg::new(0, 0xd84);
pub const CONTROL: DfReg<Control::Register> = DfReg::new(0, 0xd88);
pub const EXTENSION: DfReg = DfReg::new(0, 0xd8c);
pub const STRIDE: u16 = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub base: u64,
    pub end: u64,
    pub destination: u16,
    pub cpu_disabled: bool,
    pub nonposted: bool,
}
impl Route {
    pub fn decode(
        base: u32,
        limit: u32,
        control: u32,
        extension: u32,
    ) -> Result<Option<Self>, super::Error> {
        if control == u32::MAX {
            return Err(super::Error::Unavailable);
        }
        let c = LocalRegisterCopy::<u32, Control::Register>::new(control);
        match (c.is_set(Control::READ), c.is_set(Control::WRITE)) {
            (false, false) => return Ok(None),
            (true, true) => {}
            _ => return Err(super::Error::InvalidRoute),
        }
        let x = LocalRegisterCopy::<u32, Extension::Register>::new(extension);
        let base = (u64::from(base) << 16) | (u64::from(x.read(Extension::BASE_HIGH)) << 48);
        let end =
            ((u64::from(limit) << 16) | (u64::from(x.read(Extension::LIMIT_HIGH)) << 48) | 0xffff)
                + 1;
        if base >= end {
            return Err(super::Error::InvalidRoute);
        }
        Ok(Some(Self {
            base,
            end,
            destination: c.read(Control::DESTINATION) as u16,
            cpu_disabled: c.is_set(Control::CPU_DISABLE),
            nonposted: c.is_set(Control::NONPOSTED),
        }))
    }
}
/// Complete raw rule; reserved bits are retained when claiming an unused pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    pub base: u32,
    pub limit: u32,
    pub control: u32,
    pub extension: u32,
}
pub type Table = [Raw; PAIRS as usize];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    InvalidRequest,
    InvalidTable,
    Overlap,
    Exhausted,
    Stale,
    Readback,
}
pub fn snapshot<P: fstart_pci::ConfigRegionAccess>(
    df: &super::Df<P>,
) -> Result<Table, InstallError> {
    use tock_registers::interfaces::Readable;
    let mut table = [Raw::default(); PAIRS as usize];
    for pair in 0..PAIRS {
        let raw = Raw {
            base: df.reg(BASE.at(pair, STRIDE)).get(),
            limit: df.reg(LIMIT.at(pair, STRIDE)).get(),
            control: df.reg(CONTROL.at(pair, STRIDE)).get(),
            extension: df.reg(EXTENSION.at(pair, STRIDE)).get(),
        };
        Route::decode(raw.base, raw.limit, raw.control, raw.extension)
            .map_err(|_| InstallError::InvalidTable)?;
        table[pair as usize] = raw;
    }
    Ok(table)
}
/// One-shot claim of a previously unused pair. Never relocates/splits an
/// existing route or silently changes its destination/compatibility policy.
pub struct Claim {
    before: Table,
    after: Raw,
    pair: u8,
}
impl Claim {
    pub fn prepare(
        before: &Table,
        base: u64,
        size: u64,
        destination: u16,
    ) -> Result<Self, InstallError> {
        let end = base.checked_add(size).ok_or(InstallError::InvalidRequest)?;
        if size == 0
            || base & 0xffff != 0
            || size & 0xffff != 0
            || end > 1 << 56
            || destination > 0xfff
        {
            return Err(InstallError::InvalidRequest);
        }
        let mut free = None;
        for (pair, raw) in before.iter().enumerate() {
            match Route::decode(raw.base, raw.limit, raw.control, raw.extension)
                .map_err(|_| InstallError::InvalidTable)?
            {
                Some(r) if base < r.end && r.base < end => return Err(InstallError::Overlap),
                None if free.is_none() => free = Some(pair as u8),
                _ => {}
            }
        }
        let pair = free.ok_or(InstallError::Exhausted)?;
        let old = before[pair as usize];
        let last = end - 1;
        let fields = Control::READ::SET
            + Control::WRITE::SET
            + Control::CPU_DISABLE::CLEAR
            + Control::NONPOSTED::CLEAR
            + Control::DESTINATION.val(u32::from(destination));
        let x = Extension::BASE_HIGH.val((base >> 48) as u32)
            + Extension::LIMIT_HIGH.val((last >> 48) as u32);
        let after = Raw {
            base: (base >> 16) as u32,
            limit: (last >> 16) as u32,
            control: fields.modify(old.control),
            extension: x.modify(old.extension),
        };
        Ok(Self {
            before: *before,
            after,
            pair,
        })
    }
    pub fn pair(&self) -> u8 {
        self.pair
    }
    /// # Safety
    /// BSP-exclusive DF4 ownership, matching socket/node; claimed range must
    /// be reserved MMIO (not RAM or another device aperture), destination a
    /// verified live IOS. Keep it reserved until reset. A readback failure
    /// consumes the attempt: do not retry/reassign its range without reset.
    pub unsafe fn install<P: fstart_pci::ConfigRegionAccess>(
        self,
        df: &super::Df<P>,
    ) -> Result<(), InstallError> {
        use tock_registers::interfaces::{Readable, Writeable};
        if snapshot(df)? != self.before {
            return Err(InstallError::Stale);
        }
        let control = df.reg(CONTROL.at(self.pair, STRIDE));
        // Disable before address changes; enable LAST. No other rule touched.
        control.set(
            (Control::READ::CLEAR + Control::WRITE::CLEAR)
                .modify(self.before[self.pair as usize].control),
        );
        df.reg(BASE.at(self.pair, STRIDE)).set(self.after.base);
        df.reg(LIMIT.at(self.pair, STRIDE)).set(self.after.limit);
        df.reg(EXTENSION.at(self.pair, STRIDE))
            .set(self.after.extension);
        control.set(self.after.control);
        let mut expected = self.before;
        expected[self.pair as usize] = self.after;
        if snapshot(df) != Ok(expected) {
            control.set((Control::READ::CLEAR + Control::WRITE::CLEAR).modify(control.get()));
            return Err(InstallError::Readback);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::{Readable, Writeable};
    #[test]
    fn installs_only_one_unused_pair_with_enables_last_and_exact_full_readback() {
        let world = super::super::super::testing::DfSpace::default();
        let df = unsafe { super::super::Df::new(&world) };
        df.reg(CONTROL).set(0xa0200010);
        df.reg(EXTENSION).set(0x12003400);
        let before = snapshot(&df).unwrap();
        world.writes.borrow_mut().clear();
        let claim = Claim::prepare(&before, 0xb1000000, 0x1000000, 0x20).unwrap();
        assert_eq!(claim.pair(), 0);
        unsafe { claim.install(&df) }.unwrap();
        let after = snapshot(&df).unwrap();
        assert_eq!(&before[1..], &after[1..]);
        assert_eq!(
            after[0],
            Raw {
                base: 0xb100,
                limit: 0xb1ff,
                control: 0xa0200013,
                extension: 0x12003400
            }
        );
        let writes = world.writes.borrow();
        assert_eq!(writes.len(), 5);
        assert_eq!(writes[0].0, (0, 0xd88, None));
        assert_eq!(writes[0].1 & 3, 0);
        assert_eq!(writes[4].0, (0, 0xd88, None));
        assert_eq!(writes[4].1 & 3, 3);
    }
    #[test]
    fn no_overlapping_partial_stale_or_unaligned_route_can_be_claimed() {
        let world = super::super::super::testing::DfSpace::default();
        let df = unsafe { super::super::Df::new(&world) };
        let table = snapshot(&df).unwrap();
        assert!(Claim::prepare(&table, 1, 0x10000, 0x20).is_err());
        assert!(Claim::prepare(&table, 0x10000, 1, 0x20).is_err());
        assert!(Claim::prepare(&table, u64::MAX - 0xffff, 0x10000, 0x20).is_err());
        assert!(Claim::prepare(&table, 0, 0x10000, 0x1000).is_err());
        let plan = Claim::prepare(&table, 0xb1000000, 0x1000000, 0x20).unwrap();
        df.reg(CONTROL.at(1, STRIDE)).set(0x220000);
        world.writes.borrow_mut().clear();
        assert_eq!(unsafe { plan.install(&df) }, Err(InstallError::Stale));
        assert!(world.writes.borrow().is_empty());
        let mut table = table;
        table[2] = Raw {
            base: 0xb000,
            limit: 0xb1ff,
            control: 0x220003,
            extension: 0,
        };
        assert!(matches!(
            Claim::prepare(&table, 0xb1000000, 0x10000, 0x20),
            Err(InstallError::Overlap)
        ));
        table[2].control = 1;
        assert!(matches!(
            Claim::prepare(&table, 0xb1000000, 0x10000, 0x20),
            Err(InstallError::InvalidTable)
        ));
    }
    #[test]
    fn exhausted_and_failed_write_are_fail_closed() {
        let mut table = [Raw::default(); PAIRS as usize];
        for (i, r) in table.iter_mut().enumerate() {
            *r = Raw {
                base: i as u32,
                limit: i as u32,
                control: 0x220003,
                extension: 0,
            };
        }
        assert!(matches!(
            Claim::prepare(&table, 0xb1000000, 0x10000, 0x20),
            Err(InstallError::Exhausted)
        ));
        struct RejectEnable(super::super::super::testing::DfSpace);
        impl fstart_pci::ConfigRegionAccess for RejectEnable {
            unsafe fn read(&self, a: fstart_pci::PciAddress, o: u16) -> u32 {
                unsafe { fstart_pci::ConfigRegionAccess::read(&self.0, a, o) }
            }
            unsafe fn write(&self, a: fstart_pci::PciAddress, o: u16, v: u32) {
                if o != 0xd88 || v & 3 != 3 {
                    unsafe { fstart_pci::ConfigRegionAccess::write(&self.0, a, o, v) }
                }
            }
        }
        let world = RejectEnable(Default::default());
        let df = unsafe { super::super::Df::new(&world) };
        let table = snapshot(&df).unwrap();
        let plan = Claim::prepare(&table, 0xb1000000, 0x1000000, 0x20).unwrap();
        assert_eq!(unsafe { plan.install(&df) }, Err(InstallError::Readback));
        assert_eq!(df.reg(CONTROL).get() & 3, 0);
    }
    #[test]
    fn decodes_inclusive_64k_granularity_and_all_twelve_destination_bits() {
        assert_eq!(
            Route::decode(0xb000, 0xb0ff, 0xabc0003, 0).unwrap(),
            Some(Route {
                base: 0xb0000000,
                end: 0xb1000000,
                destination: 0xabc,
                cpu_disabled: false,
                nonposted: false
            })
        );
        let r = Route::decode(0, 0xffffffff, 0x22000f, 0x00020002)
            .unwrap()
            .unwrap();
        assert_eq!(r.base, 2 << 48);
        assert_eq!(r.end, 3 << 48);
        assert!(r.cpu_disabled && r.nonposted);
    }
    #[test]
    fn refuses_partial_unavailable_and_reversed_rules_but_ignores_disabled_garbage() {
        assert!(Route::decode(0, 0, 1, 0).is_err());
        assert!(Route::decode(0, 0, u32::MAX, 0).is_err());
        assert!(Route::decode(2, 1, 3, 0).is_err());
        assert_eq!(Route::decode(u32::MAX, 0, 0, u32::MAX), Ok(None));
    }
}
