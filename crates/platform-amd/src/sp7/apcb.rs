// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ABL tokens of an SP7 board's APCB that its [`Config`] must agree
//! with. The ABL reads them before x86 runs; the board sets them at image
//! build time (`[package.metadata.fstart.amd] apcb-tokens`, image-build's
//! `amd::apcb_tokens`, docs/amd-apcb.md) and checks them against its
//! configuration in a host test ([`disagreement`]).
//!
//! - ECAM: `DfPciMmioBaseLo/Hi`, `DfPciMmioSize` (2P_COMPLETE §2.2). The
//!   ABL places the PCI configuration window there and the vendor's DXE
//!   asserts that `PcdPciExpressBaseAddress` equals them
//!   (`FabricApcbPcieMmioBaseAddressCheck`); fstart programs
//!   [`Config::ecam`] when the ABL left `MMIO_CFG_BASE` off.
//! - IOMMU: `NbioIommu`, the setup option GIGABYTE syncs from the same CBS
//!   field (`CbsCmnGnbNbIOMMU`) as `PcdCfgIommuSupport`, which fstart's
//!   `nbio::wh::Policy::iommu` is (VALUES.md, both donors 1).

use super::Config;
use fstart_driver_amd::ccx::msr::EcamWindow;

/// Token IDs (`fstart_amdfw_format::apcb::names`).
pub mod token {
    /// `DfPciMmioBaseLo`: bits 31:0 of the ECAM base.
    pub const DF_PCI_MMIO_BASE_LO: u32 = 0x7a4f_a9ed;
    /// `DfPciMmioBaseHi`: bits 63:32 of the ECAM base.
    pub const DF_PCI_MMIO_BASE_HI: u32 = 0xf6b9_15bf;
    /// `DfPciMmioSize`: the ECAM's bytes, 256 MiB per segment
    /// (`DfDbgNumPciSegments` "1/2/4 Segments").
    pub const DF_PCI_MMIO_SIZE: u32 = 0x3d9b_7d7b;
    /// `NbioIommu`: 1 IOMMU on.
    pub const NBIO_IOMMU: u32 = 0x1ccc_9834;
}

/// The `DfPciMmio*` tokens `(id, value)` that place the ECAM at `window`.
pub const fn ecam_tokens(window: EcamWindow) -> [(u32, u32); 3] {
    [
        (token::DF_PCI_MMIO_BASE_LO, window.base as u32),
        (token::DF_PCI_MMIO_BASE_HI, (window.base >> 32) as u32),
        (token::DF_PCI_MMIO_SIZE, window.size() as u32),
    ]
}

/// The ECAM window of the `DfPciMmio*` token values, if they name one.
pub const fn ecam_of(base_lo: u32, base_hi: u32, size: u32) -> Option<EcamWindow> {
    let bytes = size as u64;
    if bytes == 0 || !bytes.is_multiple_of(256 << 20) || bytes / (256 << 20) > u8::MAX as u64 {
        return None;
    }
    EcamWindow::new(
        (base_hi as u64) << 32 | base_lo as u64,
        (bytes / (256 << 20)) as u8,
    )
}

/// The first token of the board's APCB settings (`token(id)`, `None`
/// where the board sets none) that disagrees with `config`, or `None`.
/// The ECAM tokens must be there, as the ECAM fstart falls back to
/// ([`Config::ecam`], else [`super::DEFAULT_ECAM`]); `NbioIommu`, if
/// there, must be `nbio.iommu`.
pub fn disagreement(config: &Config, token: impl Fn(u32) -> Option<u32>) -> Option<u32> {
    let ecam = config.ecam.unwrap_or(super::DEFAULT_ECAM);
    if let Some(&(id, _)) = ecam_tokens(ecam)
        .iter()
        .find(|&&(id, value)| token(id) != Some(value))
    {
        return Some(id);
    }
    let iommu = token(token::NBIO_IOMMU);
    if iommu.is_some_and(|v| v != u32::from(config.nbio.iommu)) {
        return Some(token::NBIO_IOMMU);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_donors_ecam_tokens() {
        // 2P_COMPLETE §2.2: GIGABYTE `0x2FFC_8000_0000`/256 MiB, Dell
        // `0x1000_0000_0000`/512 MiB.
        let gigabyte = EcamWindow::new(0x2ffc_8000_0000, 1).unwrap();
        assert_eq!(
            ecam_tokens(gigabyte),
            [
                (0x7a4f_a9ed, 0x8000_0000),
                (0xf6b9_15bf, 0x2ffc),
                (0x3d9b_7d7b, 0x1000_0000)
            ]
        );
        assert_eq!(ecam_of(0x8000_0000, 0x2ffc, 0x1000_0000), Some(gigabyte));
        let dell = ecam_of(0, 0x1000, 0x2000_0000).unwrap();
        assert_eq!((dell.base, dell.segments), (0x1000_0000_0000, 2));
        assert_eq!(ecam_of(0, 0x1000, 0x3000_0000), None);
        assert_eq!(ecam_of(0x1000_0000, 0, 0x2000_0000), None);
        assert_eq!(ecam_of(0, 0, 0), None);
        for (id, name) in [
            (token::DF_PCI_MMIO_BASE_LO, "DfPciMmioBaseLo"),
            (token::DF_PCI_MMIO_BASE_HI, "DfPciMmioBaseHi"),
            (token::DF_PCI_MMIO_SIZE, "DfPciMmioSize"),
            (token::NBIO_IOMMU, "NbioIommu"),
        ] {
            assert_eq!(fstart_amdfw_format::apcb::names::id(name), Some(id));
        }
    }
}
