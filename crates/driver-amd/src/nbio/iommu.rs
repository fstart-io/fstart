// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO IOMMU of every die: its MMIO window and the registers the IVRS
//! describes, after the vendor AMI NbDxe and AmdNbioIOMMUZPDxe (sp4r2 report
//! NBIO_BASE_ZP 3.6 and 3.7). The IOMMU tables of the NBIO early init already
//! set up the L1s and the L2; translation stays off until the OS enables it.
//!
//! The IOMMU is function 2 of device 0 on the die's root bus. Its
//! configuration space is aliased in SMN, with the IOMMU capability at 0x40:
//! NbDxe programs the base at SMN 0x13f00044/48 (D0F2 0x44/0x48),
//! [`IOMMU_BAR`].
use super::iohc::IOMMU_BAR;
use crate::{df::Die, soc::Soc};
use crate::smn::SmnReg;
use fstart_core::mmio::MmioReadOnly;
use fstart_log::Hex;
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::Readable,
    register_bitfields, register_structs,
};

/// Offset of the IOMMU capability in D0F2's configuration space.
pub const CAPABILITY: u16 = 0x40;
const CAPABILITY_ID: u32 = 0x0f;

register_bitfields![u32,
    CapHeader [ID OFFSET(0) NUMBITS(8) []]
];
const CAP_HEADER: SmnReg<CapHeader::Register> = SmnReg::new(0x13f0_0000 + CAPABILITY as u32);
const _: () = assert!(IOMMU_BAR.low.address() == CAP_HEADER.address() + 0x04);
const RANGE: SmnReg = SmnReg::new(CAP_HEADER.address() + 0x0c);
const MISC: SmnReg = SmnReg::new(CAP_HEADER.address() + 0x10);

register_structs! {
    Mmio {
        (0x0000 => _reserved0),
        (0x0018 => control: MmioReadOnly<u64>),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The IOMMU of `die` as [`init_bar`] mapped it, `None` if it isn't mapped
/// or its MMIO doesn't decode.
///
/// # Safety
/// The IOMMU MMIO is identity mapped and uncached.
pub unsafe fn read<S: Soc, P: ConfigRegionAccess + Copy>(die: &Die<S, P>) -> Option<Iommu> {
    let smn = die.smn();
    let base = IOMMU_BAR.base(&smn)?;
    // SAFETY: the enabled IOMMU register window, per the caller.
    let mmio = unsafe { &*(base as usize as *const Mmio) };
    let iommu = Iommu {
        base,
        capability: smn.reg(CAP_HEADER).get(),
        range: smn.reg(RANGE).get(),
        misc: smn.reg(MISC).get(),
        control: mmio.control.get(),
        efr: mmio.efr.get(),
        efr2: mmio.efr2.get(),
        counter_config: mmio.counter_config.get(),
    };
    fstart_log::debug!(
        "IOMMU {}: capability {} EFR {}",
        die.index(),
        Hex(iommu.capability.into()),
        Hex(iommu.efr)
    );
    if iommu.efr == u64::MAX {
        fstart_log::error!("IOMMU {}: MMIO doesn't decode", die.index());
        return None;
    }
    Some(iommu)
}

#[cfg(all(test, feature = "zeppelin"))]
mod tests {
    use super::*;
    use crate::soc::zeppelin::test_pci::Pci;

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
        assert_eq!(unsafe { read(&dies[1]) }, None);
    }
}
