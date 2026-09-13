//! Intel GMA PCI resource descriptions.


use crate::types::{PciBdf, PhysAddr};

/// Prepared chipset resources supplied by the northbridge driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmaResources {
    /// PCI address of the IGD function.
    pub pci_bdf: PciBdf,
    /// GTTMMADR BAR base.
    pub gtt_mmio_base: PhysAddr,
    /// GTTMMADR BAR size.
    pub gtt_mmio_size: u32,
    /// Optional MMIO-visible GTT PTE table base. Older i9xx/Pineview expose
    /// this through a separate BAR; GM965/G45 use the upper part of GTTMMADR.
    pub gtt_pte_base: Option<PhysAddr>,
    /// CPU-visible graphics aperture base, if programmed.
    pub gmadr_base: Option<PhysAddr>,
    /// Graphics aperture size.
    pub gmadr_size: u32,
    /// Physical base of stolen memory.
    pub stolen_base: PhysAddr,
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
        if self.gtt_mmio_base.0 == 0 || self.gtt_mmio_size == 0 {
            return Err(crate::error::GmaError::MmioUnavailable);
        }
        if self.gmadr_size == 0 || self.stolen_size == 0 || self.gtt_size == 0 {
            return Err(crate::error::GmaError::GttSetupFailed);
        }
        Ok(())
    }
}
