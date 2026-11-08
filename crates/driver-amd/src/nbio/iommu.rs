// SPDX-License-Identifier: MIT OR Apache-2.0
//! NBIO IOMMU capability/MMIO capture, shared independently of the legacy
//! SoC/Die ownership model. Legacy initialization follows AMI NbDxe and
//! AmdNbioIOMMUZPDxe (sp4r2 report NBIO_BASE_ZP 3.6/3.7); mapping/capture
//! alone does NOT initialize L1/L2 policy or enable translation.
//!
//! The IOMMU is function 2 of device 0 on the die's root bus. Its
//! configuration space is aliased in SMN, with the IOMMU capability at 0x40:
//! NbDxe programs the base at SMN 0x13f00044/48 (D0F2 0x44/0x48),
//! [`IOMMU_BAR`].
#[cfg(feature = "rs")]
pub mod rs;
use super::iohc::IOMMU_BAR;
use crate::smn::{SmnAccess, SmnAddr, SmnReg};
use fstart_core::mmio::MmioReadOnly;
use fstart_log::Hex;
use tock_registers::{
    LocalRegisterCopy, interfaces::Readable, register_bitfields, register_structs,
};
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
use {
    crate::{df::Die, soc::Soc},
    fstart_pci::ConfigRegionAccess,
};

/// Offset of the IOMMU capability in D0F2's configuration space.
pub const CAPABILITY: u16 = 0x40;
pub const CAPABILITY_ID: u32 = 0x0f;

register_bitfields![u32,
    pub CapHeader [ID OFFSET(0) NUMBITS(8) []],
    /// The writable EFR shadow, D0F2x80 and its L2A copy (INF names).
    pub EfrShadow0 [
        XT OFFSET(2) NUMBITS(1) [],
        /// Milan SNP-style bits 4, 5 (cleared by PCD 0x1C2).
        SNP_LO OFFSET(4) NUMBITS(2) [],
        GA OFFSET(7) NUMBITS(1) [],
        GAM OFFSET(21) NUMBITS(3) []
    ],
    /// D0F2x84 and its L2A copy.
    pub EfrShadow1 [
        BIT21 OFFSET(21) NUMBITS(1) [],
        BIT22 OFFSET(22) NUMBITS(1) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ]
];
/// The IOMMU's PCI config space (D0F2) in SMN.
const L2_CONFIG: u32 = 0x13f0_0000;
pub const EFR_SHADOW0: SmnReg<EfrShadow0::Register> = SmnReg::new(L2_CONFIG + 0x80);
pub const EFR_SHADOW1: SmnReg<EfrShadow1::Register> = SmnReg::new(L2_CONFIG + 0x84);
pub const L2A_EFR_SHADOW0: SmnReg<EfrShadow0::Register> = SmnReg::new(0x1570_4330);
pub const L2A_EFR_SHADOW1: SmnReg<EfrShadow1::Register> = SmnReg::new(0x1570_4334);
pub const L2_CP_CONTROL_1: SmnReg = SmnReg::new(0x13f0_115c);
/// The L2's copy of IOHC SB_LOCATION.
pub const L2_SB_LOCATION: SmnReg = SmnReg::new(0x13f0_112c);
/// The L1 instances: PCIE0, PCIE1, NBIF, IOAGR on SSP/GN; four on ZP.
pub const L1_SSP: [SmnAddr; 4] = [0x1470_0000, 0x14b0_0000, 0x14f0_0000, 0x1530_0000];
pub const L1_ZP: [SmnAddr; 4] = [0x1470_0000, 0x1480_0000, 0x1490_0000, 0x14a0_0000];
/// An L1's copy of IOHC SB_LOCATION (the IOAGR's on SSP/GN, the second
/// L1's on RV).
pub const fn l1_sb_location(l1: SmnAddr) -> SmnReg {
    SmnReg::new(l1 + 0x24)
}
/// The IOMMU MMIO window, 512 KiB aligned.
pub const MMIO_SIZE: u64 = 512 << 10;
pub const CAP_HEADER: SmnReg<CapHeader::Register> = SmnReg::new(L2_CONFIG + CAPABILITY as u32);
const _: () = assert!(IOMMU_BAR.low.address() == CAP_HEADER.address() + 0x04);
pub const RANGE: SmnReg = SmnReg::new(CAP_HEADER.address() + 0x0c);
pub const MISC: SmnReg = SmnReg::new(CAP_HEADER.address() + 0x10);

register_bitfields![u64,
    /// IOMMU MMIO control at18h; mapping the BAR must not enable translation.
    pub Control [ENABLE OFFSET(0) NUMBITS(1) []]
];
register_structs! {
    Mmio {
        (0x0000 => _reserved0),
        (0x0018 => control: MmioReadOnly<u64, Control::Register>),
        (0x0020 => _reserved1),
        (0x0030 => efr: MmioReadOnly<u64>),
        (0x0038 => _reserved2),
        (0x01a0 => efr2: MmioReadOnly<u64>),
        (0x01a8 => _reserved3),
        (0x4000 => counter_config: MmioReadOnly<u64>),
        (0x4008 => @END),
    }
}

/// Map the 512 KiB of IOMMU registers of `die` at `base` (512 KiB aligned,
/// outside the PCI windows so nothing moves it) and return it, `None`
/// without an IOMMU.
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub fn init_bar<S: Soc, P: ConfigRegionAccess + Copy>(die: &Die<S, P>, base: u32) -> Option<u32> {
    let smn = die.smn();
    if smn.reg(CAP_HEADER).read(CapHeader::ID) != CAPABILITY_ID {
        fstart_log::error!("IOMMU {}: no IOMMU capability", die.index());
        return None;
    }
    IOMMU_BAR.map(&smn, base);
    fstart_log::info!("IOMMU {}: MMIO at {}", die.index(), Hex(base.into()));
    Some(base)
}

/// What the IVRS describes of an IOMMU: the capability header, range and
/// miscellaneous information 0 registers and MMIO 0x18 (control), 0x30
/// (EFR), 0x1a0 (EFR2) and 0x4000 (counter configuration).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Iommu {
    pub base: u64,
    pub capability: u32,
    pub range: u32,
    pub misc: u32,
    pub control: u64,
    pub efr: u64,
    pub efr2: u64,
    pub counter_config: u64,
}

impl Iommu {
    pub fn translation_enabled(&self) -> bool {
        LocalRegisterCopy::<u64, Control::Register>::new(self.control).is_set(Control::ENABLE)
    }
}

/// Capture an enabled IOMMU through the caller's checked IOHC SMN transport,
/// `None` if its BAR isn't enabled or its MMIO doesn't decode.
///
/// # Safety
/// The IOMMU MMIO is identity mapped and uncached.
pub unsafe fn read(smn: &impl SmnAccess) -> Option<Iommu> {
    let base = IOMMU_BAR.base(smn)?;
    // SAFETY: the caller's decoded, UC, identity-mapped window contract.
    unsafe {
        capture(
            base,
            smn.reg(CAP_HEADER).get(),
            smn.reg(RANGE).get(),
            smn.reg(MISC).get(),
        )
    }
}

/// Capture the common MMIO block with capability metadata read through
/// either the correct instance's SMN view or its own PCI capability.
///
/// # Safety
/// `base..base+MMIO_SIZE` is the decoded IOMMU register window, identity
/// mapped and uncached, with exclusive access for these register reads.
pub unsafe fn capture(base: u64, capability: u32, range: u32, misc: u32) -> Option<Iommu> {
    // SAFETY: the enabled IOMMU register window, per the caller.
    let mmio = unsafe { &*(base as usize as *const Mmio) };
    let iommu = Iommu {
        base,
        capability,
        range,
        misc,
        control: mmio.control.get(),
        efr: mmio.efr.get(),
        efr2: mmio.efr2.get(),
        counter_config: mmio.counter_config.get(),
    };
    fstart_log::debug!(
        "IOMMU base {}: capability {} EFR {}",
        Hex(base),
        Hex(iommu.capability.into()),
        Hex(iommu.efr)
    );
    if iommu.efr == u64::MAX {
        fstart_log::error!("IOMMU base {}: MMIO doesn't decode", Hex(base));
        return None;
    }
    Some(iommu)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "zeppelin")]
    use crate::soc::zeppelin::test_pci::Pci;

    #[test]
    fn capture_does_not_confuse_other_control_bits_with_translation() {
        let mut iommu = Iommu {
            control: 1 << 10,
            ..Default::default()
        };
        assert!(!iommu.translation_enabled());
        iommu.control |= Control::ENABLE::SET.value;
        assert!(iommu.translation_enabled());
    }

    #[cfg(feature = "zeppelin")]
    #[test]
    fn maps_each_die_with_an_iommu_in_its_pool() {
        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        let dies = pci.dies();
        pci.set_smn(0, CAP_HEADER, 0x1964_000f);
        pci.set_smn(0, IOMMU_BAR.high, 0xdead);
        assert_eq!(init_bar(&dies[0], 0xfe60_0000), Some(0xfe60_0000));
        assert_eq!(pci.smn(0, IOMMU_BAR.low), 0xfe60_0001);
        assert_eq!(pci.smn(0, IOMMU_BAR.high), 0);
        // No IOMMU capability: left alone.
        assert_eq!(init_bar(&dies[1], 0xc000_0000), None);
        assert_eq!(pci.smn(1, IOMMU_BAR.low), 0);
        assert_eq!(unsafe { read(&dies[1].smn()) }, None);
    }
}
