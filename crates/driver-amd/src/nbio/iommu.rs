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
use crate::smn::{SmnAddr, SmnReg};
use tock_registers::register_bitfields;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
use {
    crate::{df::Die, soc::Soc},
    fstart_core::mmio::MmioReadOnly,
    fstart_log::Hex,
    fstart_pci::ConfigRegionAccess,
    tock_registers::{interfaces::Readable, register_structs},
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
        BIT21_22 OFFSET(21) NUMBITS(2) [],
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

#[cfg(any(feature = "picasso", feature = "zeppelin"))]
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
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
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
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
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
