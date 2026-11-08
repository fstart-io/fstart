// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs IOD IOMMU environment policy for the measured steppings: Genoa B0 from
//! openSIL NbioIommuTbl.h and Iommul1Reg/Iommul2Reg/NbioBaseReg; Siena A2 the
//! same table as its vendor firmware leaves it: no NW (L1 EnableNw, L2
//! MaskNw) and no L2A EFR shadow, which
//! does not decode on Siena. Not the SP3 four-L1 policy: the executable table
//! names PCIE0, PCIE1 and IOAGR (IOAGR repeats are idempotent).
//! No translation, queues, interrupt remapping, SEV-SNP or AVIC are enabled.
use super::{CAP_HEADER, Iommu};
use crate::nbio::iohc;
use crate::{
    smn::{SmnAccess, SmnReg},
    table::{self, Op, Record},
};
use tock_registers::{
    RegisterLongName,
    fields::FieldValue,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    Interrupt [PIN OFFSET(0) NUMBITS(3) []],
    Misc [HT_ATS_RESERVED OFFSET(22) NUMBITS(1) []],
    L1Control4 [
        MULTIPLE_ATS OFFSET(0) NUMBITS(1) [],
        TIMEOUT_EXTEND OFFSET(2) NUMBITS(1) []
    ],
    L1Misc [
        FORCE_INVALID_FILTER OFFSET(6) NUMBITS(1) [],
        DISABLE_4K_WAIT OFFSET(12) NUMBITS(1) [],
        ENABLE_NW OFFSET(17) NUMBITS(1) []
    ],
    L1Feature [
        PMR_LOCK OFFSET(1) NUMBITS(1) [],
        EXE_LOCK OFFSET(2) NUMBITS(1) []
    ],
    L1Control2 [
        RESPONSE_MODE OFFSET(24) NUMBITS(3) [],
        NONCONSUMED_ERROR OFFSET(27) NUMBITS(1) [],
        CONSUMED_ERROR OFFSET(28) NUMBITS(1) []
    ],
    L1Wake [DMA OFFSET(0) NUMBITS(1) []],
    L1Clock [
        DMA OFFSET(4) NUMBITS(1) [],
        CACHE OFFSET(5) NUMBITS(1) [],
        SLAVE OFFSET(6) NUMBITS(1) [],
        PERF OFFSET(8) NUMBITS(1) [],
        MEMORY OFFSET(9) NUMBITS(1) [],
        REG OFFSET(10) NUMBITS(1) [],
        HOSTREQ OFFSET(11) NUMBITS(1) [],
        DMARSP OFFSET(12) NUMBITS(1) [],
        HOSTRSP OFFSET(13) NUMBITS(1) [],
        SION OFFSET(14) NUMBITS(1) [],
        HYSTERESIS OFFSET(15) NUMBITS(8) [],
        L2 OFFSET(31) NUMBITS(1) []
    ],
    L1Support [XT OFFSET(5) NUMBITS(1) []],
    CreditMode [RELOCATE OFFSET(31) NUMBITS(1) []],
    Credit7 [VC7 OFFSET(24) NUMBITS(8) []],
    ReadCredit [VC4 OFFSET(16) NUMBITS(4) []],
    WriteCredit [
        VC4 OFFSET(16) NUMBITS(4) [],
        VC6 OFFSET(24) NUMBITS(4) []
    ],
    ErrorRule [LOCK OFFSET(0) NUMBITS(1) []],
    PageSize [
        GUEST OFFSET(0) NUMBITS(7) [],
        HOST OFFSET(8) NUMBITS(7) []
    ],
    Control5 [PARTIAL_GUEST OFFSET(12) NUMBITS(7) []],
    L2Misc [
        ATS_IW OFFSET(0) NUMBITS(1) [],
        MASK_NW OFFSET(1) NUMBITS(1) [],
        ATOMIC_FILTER OFFSET(2) NUMBITS(1) []
    ],
    Command [
        FLUSH_WAIT OFFSET(1) NUMBITS(1) [],
        FLUSH_INVALIDATE OFFSET(2) NUMBITS(1) []
    ],
    Filter [BYPASS OFFSET(0) NUMBITS(1) []],
    Walker [
        COHERENT OFFSET(6) NUMBITS(1) [],
        PREFETCH OFFSET(8) NUMBITS(1) [],
        RANGE OFFSET(12) NUMBITS(3) [],
        NESTED OFFSET(25) NUMBITS(1) []
    ],
    Shadow0 [
        SNP_AVIC OFFSET(18) NUMBITS(3) [],
        GAM OFFSET(21) NUMBITS(3) []
    ],
    Shadow1 [
        INVALIDATE_TYPE OFFSET(19) NUMBITS(1) [],
        SNP OFFSET(31) NUMBITS(1) []
    ],
    ClockA [
        REG_DISABLE OFFSET(0) NUMBITS(1) [],
        DYNAMIC_DISABLE OFFSET(1) NUMBITS(1) [],
        CACHE_DISABLE OFFSET(2) NUMBITS(1) []
    ],
    ClockB [
        REG_DISABLE OFFSET(0) NUMBITS(1) [],
        DYNAMIC_DISABLE OFFSET(1) NUMBITS(1) [],
        MISC_DISABLE OFFSET(2) NUMBITS(1) [],
        CACHE_DISABLE OFFSET(3) NUMBITS(1) []
    ]
];
const L1: [u32; 3] = [0x14700000, 0x14b00000, 0x15300000];
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    Identity,
    Instance,
    Unavailable,
    Readback,
    Capacity,
}
pub struct Plan {
    offset: u32,
    fch: bool,
    changes: heapless::Vec<Record<SmnReg, u32>, 64>,
}
fn update<R: RegisterLongName>(address: u32, fields: FieldValue<u32, R>) -> Record<SmnReg, u32> {
    table::rmw(SmnReg::new(address), fields.mask(), fields.value)
}
/// The CPU steppings this policy was measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stepping {
    GenoaB0,
    SienaA2,
}
impl Stepping {
    const fn of(signature: u32) -> Option<Self> {
        match signature {
            0x00a1_0f10 => Some(Self::GenoaB0),
            0x00aa_0f02 => Some(Self::SienaA2),
            _ => None,
        }
    }
}
impl Plan {
    /// Only the measured Genoa B0 and Siena A2 CPUs are supported. A0's
    /// ordering workaround must not be silently applied to another stepping.
    /// Clock gating and actual FCH ownership are explicit; preflight checks
    /// its live SB_LOCATION.
    pub fn new(
        physical_ios: u8,
        signature: u32,
        clock_gating: bool,
        fch: bool,
    ) -> Result<Self, Error> {
        let Some(stepping) = Stepping::of(signature) else {
            return Err(Error::Identity);
        };
        if physical_ios >= 4 {
            return Err(Error::Identity);
        }
        let genoa = stepping == Stepping::GenoaB0;
        let mut changes = heapless::Vec::new();
        let mut add = |record| changes.push(record).map_err(|_| Error::Capacity);
        add(update(0x13f0007c, Interrupt::PIN.val(0)))?;
        add(update(0x13f00050, Misc::HT_ATS_RESERVED::CLEAR))?;
        for base in L1 {
            add(update(
                base + 0xc8,
                L1Control4::MULTIPLE_ATS::SET + L1Control4::TIMEOUT_EXTEND::SET,
            ))?;
            add(update(
                base + 0x1c,
                L1Misc::FORCE_INVALID_FILTER::SET
                    + L1Misc::DISABLE_4K_WAIT::SET
                    + L1Misc::ENABLE_NW.val(genoa.into()),
            ))?;
            add(update(
                base + 0x9c,
                L1Feature::PMR_LOCK::SET + L1Feature::EXE_LOCK::SET,
            ))?;
            add(update(
                base + 0x38,
                L1Control2::RESPONSE_MODE.val(1)
                    + L1Control2::NONCONSUMED_ERROR::SET
                    + L1Control2::CONSUMED_ERROR::SET,
            ))?;
            add(update(base + 0xd4, L1Wake::DMA::SET))?;
        }
        add(update(0x1530010c, CreditMode::RELOCATE::SET))?;
        add(update(0x15300118, Credit7::VC7.val(1)))?;
        add(update(0x15300120, Credit7::VC7.val(1)))?;
        add(update(0x15300124, ReadCredit::VC4.val(1)))?;
        add(update(
            0x15300128,
            WriteCredit::VC4.val(1) + WriteCredit::VC6.val(1),
        ))?;
        add(update(0x1530010c, CreditMode::RELOCATE::CLEAR))?;
        add(update(0x13f01200, ErrorRule::LOCK::SET))?;
        add(update(0x157000c0, ErrorRule::LOCK::SET))?;
        add(update(
            0x157000d0,
            PageSize::GUEST.val(0x49) + PageSize::HOST.val(0x49),
        ))?;
        add(update(
            0x13f01250,
            PageSize::GUEST.val(0x49) + PageSize::HOST.val(0x49),
        ))?;
        add(update(0x13f01130, Control5::PARTIAL_GUEST.val(0x49)))?;
        add(update(
            0x13f0111c,
            L2Misc::ATS_IW::SET + L2Misc::MASK_NW.val(genoa.into()) + L2Misc::ATOMIC_FILTER::SET,
        ))?;
        add(update(
            0x13f01158,
            Command::FLUSH_WAIT::SET + Command::FLUSH_INVALIDATE::CLEAR,
        ))?;
        add(update(0x13f0114c, Filter::BYPASS::CLEAR))?;
        add(update(0x15700088, Filter::BYPASS::CLEAR))?;
        add(update(
            0x13f01150,
            Walker::COHERENT::SET
                + Walker::PREFETCH::SET
                + Walker::RANGE.val(7)
                + Walker::NESTED::SET,
        ))?;
        // Native CPU policy does not establish SEV-SNP or AVIC. Do not expose
        // those virtualization modes merely because reset shadows support them.
        add(update(
            0x13f00084,
            Shadow1::INVALIDATE_TYPE::CLEAR + Shadow1::SNP::CLEAR,
        ))?;
        add(update(
            0x13f00080,
            Shadow0::SNP_AVIC.val(0) + Shadow0::GAM.val(0),
        ))?;
        if genoa {
            add(update(
                0x15704330,
                Shadow0::SNP_AVIC.val(0) + Shadow0::GAM.val(0),
            ))?;
            add(update(0x15704334, Shadow1::SNP::CLEAR))?;
        }
        let off = u32::from(!clock_gating);
        add(update(
            0x157000cc,
            ClockA::REG_DISABLE.val(off)
                + ClockA::DYNAMIC_DISABLE.val(off)
                + ClockA::CACHE_DISABLE.val(off),
        ))?;
        add(update(
            0x13f01240,
            ClockB::REG_DISABLE.val(off)
                + ClockB::DYNAMIC_DISABLE.val(off)
                + ClockB::MISC_DISABLE.val(off)
                + ClockB::CACHE_DISABLE.val(off),
        ))?;
        for base in L1 {
            let on = u32::from(clock_gating);
            add(update(
                base + 0xcc,
                L1Clock::DMA.val(on)
                    + L1Clock::CACHE.val(on)
                    + L1Clock::SLAVE.val(on)
                    + L1Clock::PERF.val(on)
                    + L1Clock::MEMORY.val(on)
                    + L1Clock::REG.val(on)
                    + L1Clock::HOSTREQ.val(on)
                    + L1Clock::DMARSP.val(on)
                    + L1Clock::HOSTRSP.val(on)
                    + L1Clock::SION.val(on)
                    + L1Clock::L2.val(on)
                    + L1Clock::HYSTERESIS.val(0x20),
            ))?;
        }
        add(update(0x153000dc, L1Support::XT::SET))?;
        Ok(Self {
            offset: u32::from(physical_ios) << 20,
            fch,
            changes,
        })
    }
    /// Read-only, including a unique live PCI/SMN BAR identity proof. Run on
    /// ALL units before any policy writes; root-bus selection alone is invalid.
    pub fn preflight(&self, smn: &impl SmnAccess, mapped: &Iommu) -> Result<(), Error> {
        use crate::nbio::iohc::IOMMU_BAR;
        if smn.reg(CAP_HEADER.offset(self.offset)).get() != mapped.capability
            || IOMMU_BAR.base_registers(
                &smn.reg(IOMMU_BAR.low.offset(self.offset)),
                &smn.reg(IOMMU_BAR.high.offset(self.offset)),
            ) != Some(mapped.base)
            || mapped.translation_enabled()
        {
            return Err(Error::Instance);
        }
        let location = smn.reg(iohc::SB_LOCATION.offset(self.offset)).get();
        fstart_log::info!(
            "rs: cold IOMMU IOS={} FCH={} SB={:#x} IOAGR-L1-SB={:#x} L2-SB={:#x}",
            self.offset >> 20,
            self.fch,
            location,
            smn.reg(super::l1_sb_location(L1[2]).offset(self.offset))
                .get(),
            smn.reg(super::L2_SB_LOCATION.offset(self.offset)).get()
        );
        if location == u32::MAX || (self.fch && location == 0) {
            return Err(Error::Unavailable);
        }
        for reg in [
            super::l1_sb_location(L1[2]).offset(self.offset),
            super::L2_SB_LOCATION.offset(self.offset),
        ] {
            if smn.reg(reg).get() == u32::MAX {
                return Err(Error::Unavailable);
            }
        }
        for change in &self.changes {
            if smn.reg(change.reg.offset(self.offset)).get() == u32::MAX {
                return Err(Error::Unavailable);
            }
        }
        Ok(())
    }
    /// # Safety
    /// BSP-exclusive checked Genoa B0 SMN, all units preflighted, translation
    /// disabled and all DMA consumers quiescent (call BEFORE USB starts).
    /// Any rejection poisons initialization: no retry, fallback or handoff.
    pub unsafe fn install(&self, smn: &impl SmnAccess) -> Result<(), Error> {
        // NbioBaseConfigurationGenoa, before feature/PMR locks. Only the
        // FCH-owning root forwards its live SB_LOCATION to IOAGR L1 and L2.
        // Other instances must not classify requests as this FCH.
        let source = smn.reg(iohc::SB_LOCATION.offset(self.offset));
        let location = if self.fch { source.get() } else { 0 };
        if self.fch && (location == 0 || location == u32::MAX) {
            return Err(Error::Unavailable);
        }
        if !self.fch {
            source.set(0);
        }
        if source.get() != location {
            return Err(Error::Readback);
        }
        for reg in [
            super::l1_sb_location(L1[2]).offset(self.offset),
            super::L2_SB_LOCATION.offset(self.offset),
        ] {
            smn.reg(reg).set(location);
            if smn.reg(reg).get() != location {
                return Err(Error::Readback);
            }
        }
        for change in &self.changes {
            let reg = change.reg.offset(self.offset);
            let Op::Update { clear, set } = change.op else {
                return Err(Error::Identity);
            };
            table::apply([table::rmw(reg, clear, set)], |r| smn.reg(r));
            if smn.reg(reg).get() & clear != set {
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
    #[test]
    fn stepping_and_root_are_explicit() {
        assert!(Plan::new(4, 0xa10f10, true, false).is_err());
        assert!(Plan::new(0, 0xa00f10, true, false).is_err());
        assert!(Plan::new(0, 0xaa0f01, true, false).is_err());
    }
    #[test]
    fn siena_matches_the_nfx_state_without_nw_or_the_l2a_shadow() {
        let plan = Plan::new(3, 0xaa0f02, true, false).unwrap();
        assert!(
            plan.changes
                .iter()
                .all(|c| !matches!(c.reg.address(), 0x15704330 | 0x15704334))
        );
        // The Siena vendor firmware's values satisfy every Siena record.
        let model = SmnSpace::default();
        for (address, value) in [
            (0x1470001c, 0x5040),
            (0x13f0111c, 0x60005),
            (0x157000cc, 0x50000),
            (0x13f01240, 0x50000),
            (0x147000cc, 0x80107f70),
        ] {
            model.set(SmnReg::<()>::new(address + 0x300000), value);
        }
        unsafe { plan.install(&model.smn()) }.unwrap();
        for (address, value) in [
            (0x1470001c, 0x5040),
            (0x13f0111c, 0x60005),
            (0x157000cc, 0x50000),
            (0x13f01240, 0x50000),
            (0x147000cc, 0x80107f70),
        ] {
            assert_eq!(model.get(SmnReg::<()>::new(address + 0x300000)), value);
        }
        let genoa = Plan::new(3, 0xa10f10, true, false).unwrap();
        unsafe { genoa.install(&model.smn()) }.unwrap();
        assert_eq!(model.get(SmnReg::<()>::new(0x1470001c + 0x300000)), 0x25040);
    }
    #[test]
    fn preflight_proves_the_relocated_bar_not_a_root_bus_alias() {
        use crate::nbio::iohc::IOMMU_BAR;
        let model = SmnSpace::default();
        let plan = Plan::new(2, 0xa10f10, true, true).unwrap();
        model.set(iohc::SB_LOCATION.offset(0x200000), 0x40001);
        let mapped = Iommu {
            base: 0xd8200000,
            capability: 0x190b640f,
            ..Default::default()
        };
        model.set(CAP_HEADER, mapped.capability);
        model.set(IOMMU_BAR.low, 0xd8000001);
        assert_eq!(plan.preflight(&model.smn(), &mapped), Err(Error::Instance));
        model.set(CAP_HEADER.offset(0x200000), mapped.capability);
        model.set(IOMMU_BAR.low.offset(0x200000), 0xd8200001);
        assert_eq!(plan.preflight(&model.smn(), &mapped), Ok(()));
    }
    #[test]
    fn fch_location_reaches_only_its_relocated_instance() {
        let model = SmnSpace::default();
        for index in 0..4 {
            model.set(iohc::SB_LOCATION.offset(index << 20), 0x40001);
        }
        for index in 0..4 {
            let plan = Plan::new(index, 0xa10f10, true, index == 2).unwrap();
            unsafe { plan.install(&model.smn()) }.unwrap();
            let expected = if index == 2 { 0x40001 } else { 0 };
            let offset = u32::from(index) << 20;
            assert_eq!(model.get(iohc::SB_LOCATION.offset(offset)), expected);
            assert_eq!(
                model.get(super::super::l1_sb_location(L1[2]).offset(offset)),
                expected
            );
            assert_eq!(
                model.get(super::super::L2_SB_LOCATION.offset(offset)),
                expected
            );
        }
        let missing = SmnSpace::default();
        assert_eq!(
            unsafe {
                Plan::new(2, 0xa10f10, true, true)
                    .unwrap()
                    .install(&missing.smn())
            },
            Err(Error::Unavailable)
        );
        assert_eq!(missing.get(SmnReg::<()>::new(0x1550009c)), 0);
    }
    #[test]
    fn typed_sequence_keeps_credit_transition_and_uses_three_genoa_l1s() {
        let plan = Plan::new(2, 0xa10f10, true, false).unwrap();
        assert_eq!(plan.offset, 0x200000);
        let credit: heapless::Vec<_, 4> = plan
            .changes
            .iter()
            .filter(|c| c.reg.address() == 0x1530010c)
            .collect();
        assert_eq!(credit.len(), 2);
        assert!(matches!(
            credit[0].op,
            Op::Update {
                set: 0x80000000,
                ..
            }
        ));
        assert!(matches!(credit[1].op, Op::Update { set: 0, .. }));
        assert!(
            plan.changes
                .iter()
                .all(|c| !(0x14f00000..0x15300000).contains(&c.reg.address()))
        );
        let model = SmnSpace::default();
        unsafe { plan.install(&model.smn()) }.unwrap();
        for base in L1 {
            assert_eq!(model.get(SmnReg::<()>::new(base + 0x200000 + 0x9c)) & 6, 6);
        }
        assert_eq!(model.get(SmnReg::<()>::new(0x1550010c)) >> 31, 0);
    }
}
