// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH root bridge's BARs that AmdNbioDxe (Dell PR9XJ) handles at its
//! entry, before PCI enumeration, and the SMU's non-PCI BAR (DEVICES.md
//! §2.11, §2.13 [D]):
//!
//! - `EnableIommuMmio 0x5680` ([`RootBridge::enable_iommu_mmio`]): the
//!   IOMMU's MMIO decode turned on where the base is set
//!   ([`RootBridge::set_iommu_bar`] writes it without the enable);
//! - the IOMMU's SR-IOV BARs (`0x5860`, [`RootBridge::set_iommu_vf_bars`]):
//!   the 256 MiB VF BAR (capability `+0xD0/0xD4`) and the 4 MiB VF control
//!   BAR (`+0xD8/0xDC`), each only when both halves are 0 (Dell
//!   `PcdCfgNonPCIBarInitIommuVf`/`…VfCntl` = 1, enable bit 1). Only an
//!   IOMMU with SR-IOV (a vIOMMU) uses them;
//! - the SMU's 1 MiB window (`NonPciBarInitEx`, IOHC `0x13B1_02E8/02EC`,
//!   enable and lock) that HSMP's ACPI device names
//!   ([`RootBridge::smu_bar`], [`RootBridge::set_smu_bar`]).

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::{IOMMU_CAP_BASE_HI, IOMMU_CAP_BASE_LO, RootBridge};
use crate::nbio::iohc::{BarHigh, SMU_BAR, SMU_BAR_SIZE, SmuBar};
use crate::nbio::iommu::CAPABILITY as IOMMU_CAPABILITY;
use crate::smn::SmnAccess;

/// The IOMMU VF BAR (capability `+0xD0`, 64-bit) and its size.
pub const IOMMU_VF_BAR: u16 = IOMMU_CAPABILITY + 0xd0;
pub const IOMMU_VF_BAR_SIZE: u64 = 256 << 20;
/// The IOMMU VF control BAR (capability `+0xD8`, 64-bit) and its size.
pub const IOMMU_VF_CONTROL_BAR: u16 = IOMMU_CAPABILITY + 0xd8;
pub const IOMMU_VF_CONTROL_BAR_SIZE: u64 = 4 << 20;

/// Why a BAR was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarError {
    /// The root bridge has no IOMMU.
    NoIommu,
    /// The base was 0 (the vendor's status 7 for the IOMMU MMIO).
    NoBase,
    /// The BAR already holds an address.
    Taken,
    /// A misaligned base.
    Alignment,
}

impl<P: ConfigRegionAccess + Clone> RootBridge<P> {
    /// `EnableIommuMmio`: with the IOMMU's base set, rewrite the high half
    /// unchanged and set the low half's enable.
    pub fn enable_iommu_mmio(&self) -> Result<(), BarError> {
        if !self.has_iommu() {
            return Err(BarError::NoIommu);
        }
        let iommu = self.function(2);
        // SAFETY: Bus:00.2 is this root bridge's IOMMU PCI function.
        unsafe {
            let lo = self.pci.read(iommu, IOMMU_CAP_BASE_LO);
            let hi = self.pci.read(iommu, IOMMU_CAP_BASE_HI);
            if lo & !0x3fff == 0 && hi == 0 {
                return Err(BarError::NoBase);
            }
            self.pci.write(iommu, IOMMU_CAP_BASE_HI, hi);
            self.pci.write(iommu, IOMMU_CAP_BASE_LO, lo | 1);
        }
        Ok(())
    }

    /// One 64-bit IOMMU capability BAR at `offset`: `base` with the enable
    /// (bit 0), unless either half is already set.
    fn iommu_cap_bar(&self, offset: u16, base: u64, size: u64) -> Result<(), BarError> {
        if !self.has_iommu() {
            return Err(BarError::NoIommu);
        }
        if base == 0 || !base.is_multiple_of(size) {
            return Err(BarError::Alignment);
        }
        let iommu = self.function(2);
        // SAFETY: Bus:00.2 is this root bridge's IOMMU PCI function.
        unsafe {
            if self.pci.read(iommu, offset) != 0 || self.pci.read(iommu, offset + 4) != 0 {
                return Err(BarError::Taken);
            }
            self.pci.write(iommu, offset + 4, (base >> 32) as u32);
            self.pci.write(iommu, offset, base as u32 | 1);
        }
        Ok(())
    }

    /// The IOMMU's VF BAR at `vf` (256 MiB aligned) and, if `control`, its
    /// VF control BAR (4 MiB aligned), as `0x5860` does.
    pub fn set_iommu_vf_bars(&self, vf: u64, control: Option<u64>) -> Result<(), BarError> {
        self.iommu_cap_bar(IOMMU_VF_BAR, vf, IOMMU_VF_BAR_SIZE)?;
        if let Some(control) = control {
            self.iommu_cap_bar(IOMMU_VF_CONTROL_BAR, control, IOMMU_VF_CONTROL_BAR_SIZE)?;
        }
        Ok(())
    }

    /// The SMU's window, if decoded (the vendor's HSMP reads the low half,
    /// which must have bit 0, and the high half, and masks bits 1:0).
    pub fn smu_bar(&self) -> Option<u64> {
        SMU_BAR.base(self)
    }

    /// Decode the SMU's 1 MiB window at `base`, enabled and locked, unless
    /// the BAR already holds an address.
    pub fn set_smu_bar(&self, base: u64) -> Result<(), BarError> {
        if base == 0 || !base.is_multiple_of(SMU_BAR_SIZE) {
            return Err(BarError::Alignment);
        }
        let (lo, hi) = (self.reg(SMU_BAR.low), self.reg(SMU_BAR.high));
        if lo.get() != 0 || hi.get() != 0 {
            return Err(BarError::Taken);
        }
        hi.write(BarHigh::ADDRESS.val((base >> 32) as u32));
        lo.write(SmuBar::BASE.val((base >> 20) as u32 & 0xfff));
        lo.modify(SmuBar::ENABLE::SET);
        lo.modify(SmuBar::LOCK::SET);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    #[test]
    fn smu_bar_once() {
        let space = SmnSpace::default();
        let rb = unsafe { RootBridge::new(&space, 0, 0) }.unwrap();
        assert_eq!(rb.smu_bar(), None);
        rb.set_smu_bar(0xaff0_0000).unwrap();
        assert_eq!(rb.smu_bar(), Some(0xaff0_0000));
        assert_eq!(rb.set_smu_bar(0xafe0_0000), Err(BarError::Taken));
        assert_eq!(rb.set_smu_bar(0xafe8_0000), Err(BarError::Alignment));
    }
}
