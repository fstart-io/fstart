//! Intel GMA PCI resource descriptions.

use fstart_core::typed::{Mmio32, MmioAddr};

use crate::types::PciAddress;

/// Prepared chipset resources supplied by the northbridge driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmaResources {
    /// PCI address of the IGD function.
    pub pci_bdf: PciAddress,
    /// GTTMMADR BAR base. The whole GMA register window lives here.
    pub gtt_mmio_base: MmioAddr<Mmio32>,
    /// GTTMMADR BAR size.
    pub gtt_mmio_size: u32,
    /// Optional MMIO-visible GTT PTE table base. Older i9xx/Pineview expose
    /// this through a separate BAR; GM965/G45 use the upper part of GTTMMADR.
    pub gtt_pte_base: Option<MmioAddr<Mmio32>>,
    /// CPU-visible graphics aperture base, if programmed. This is a physical
    /// scanout address, not an MMIO window.
    pub gmadr_base: Option<u64>,
    /// Graphics aperture size.
    pub gmadr_size: u32,
    /// Physical base of stolen memory.
    pub stolen_base: u64,
    /// Stolen memory size in bytes.
    pub stolen_size: u32,
    /// GTT page table size in bytes.
    pub gtt_size: u32,
    /// Optional PCI GCFGC value captured by the chipset/northbridge driver.
    ///
    /// Legacy GMCH CDClk selection lives in PCI config space rather than the
    /// display MMIO BAR. When this is unavailable the shared GMA layer falls
    /// back to conservative libgfxinit defaults.
    pub gcfgc: Option<u16>,
}

impl GmaResources {
    /// Validate the minimum resources required for framebuffer mapping.
    pub fn validate(&self) -> Result<(), crate::error::GmaError> {
        if self.gtt_mmio_base.raw() == 0 || self.gtt_mmio_size == 0 {
            return Err(crate::error::GmaError::MmioUnavailable);
        }
        if self.gmadr_size == 0 || self.stolen_size == 0 || self.gtt_size == 0 {
            return Err(crate::error::GmaError::GttSetupFailed);
        }
        Ok(())
    }
}
