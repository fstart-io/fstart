//! Shared IGD display bring-up for GMCH-generation Intel northbridges.
//!
//! This covers the parts of coreboot's `src/northbridge/intel/{i945,pineview,
//! gm965}/gma.c` that are identical across the three chipsets: assembling the
//! [`GmaResources`] the shared GMA library expects, programming the hardware
//! GTT page-table base, and handing the result to `fstart-intel-gma` for the
//! actual modeset. Register values that differ per chipset (GTT base source,
//! GTT size encoding, panel power sequence) stay in the individual drivers.
//!
//! The GTTMMADR BAR must already be programmed and enabled before any of this
//! runs; `PGETBL_CTL` points the display engine at the page table, while the
//! page-table entries themselves are written by the shared GMA library.

use fstart_core::services::framebuffer::{Framebuffer, FramebufferInfo};
#[cfg(feature = "display")]
use fstart_core::typed::mmio32;
use fstart_intel_gma::config::OutputConfig;
use fstart_intel_gma::framebuffer::FramebufferConfig;
#[cfg(feature = "display")]
use fstart_intel_gma::pci::GmaResources;
use fstart_intel_gma::types::{Cpu, PciAddress};
#[cfg(feature = "display")]
use fstart_intel_gma::{GmaInitConfig, GmaInitResult};

/// `PGETBL_CTL`: physical base and flags of the GTT page table.
pub const PGETBL_CTL: u32 = 0x02020;
/// `GFX_FLSH_CNTL`: flush register for GTT writes.
pub const GFX_FLSH_CNTL: u32 = 0x02170;
/// `PGETBL_CTL` bit that enables GTT address translation.
pub const PGETBL_ENABLED: u32 = 1;

/// Framebuffer reported before the display engine is programmed.
const NO_FRAMEBUFFER: FramebufferInfo = FramebufferInfo {
    base_addr: 0,
    width: 0,
    height: 0,
    stride: 0,
    bits_per_pixel: 0,
    red_pos: 0,
    red_size: 0,
    green_pos: 0,
    green_size: 0,
    blue_pos: 0,
    blue_size: 0,
};

/// Board display policy for one IGD northbridge.
///
/// This is board code, not chipset metadata: it names the outputs the board
/// wants lit and the framebuffer geometry it wants to hand to the payload.
#[derive(Debug, Clone, Copy)]
pub struct IgdDisplayPolicy {
    /// Outputs the board wants initialized, in preference order.
    pub outputs: &'static [OutputConfig],
    /// Framebuffer geometry and mode-selection policy.
    pub framebuffer: FramebufferConfig,
}

/// Chipset-supplied IGD addresses and sizes.
///
/// Every field is read from chipset configuration space or MMIO by the
/// northbridge driver, so the shared layer never has to know which register a
/// particular generation keeps its stolen-memory base in.
#[derive(Debug, Clone, Copy)]
pub struct IgdAddresses {
    /// PCI address of the IGD function (usually 0:2.0).
    pub pci_bdf: PciAddress,
    /// GTTMMADR BAR base: the display MMIO window.
    pub gtt_mmio_base: u64,
    /// GTTMMADR BAR size in bytes.
    pub gtt_mmio_size: u32,
    /// MMIO-visible GTT page-table base, for chipsets that expose it through a
    /// separate BAR (Pineview BAR3) instead of the top of GTTMMADR.
    pub gtt_pte_base: Option<u64>,
    /// CPU-visible graphics aperture base (GMADR), if the aperture is used.
    pub gmadr_base: Option<u64>,
    /// Graphics aperture size in bytes.
    pub gmadr_size: u32,
    /// Physical base of stolen memory.
    pub stolen_base: u64,
    /// Stolen memory size in bytes.
    pub stolen_size: u32,
    /// GTT page-table size in bytes.
    pub gtt_size: u32,
    /// `GCFGC` value: selects the chipset display clock. `None` falls back to
    /// conservative defaults.
    pub gcfgc: Option<u16>,
}

impl IgdAddresses {
    #[cfg(feature = "display")]
    fn to_gma_resources(&self) -> GmaResources {
        GmaResources {
            pci_bdf: self.pci_bdf,
            gtt_mmio_base: mmio32(self.gtt_mmio_base),
            gtt_mmio_size: self.gtt_mmio_size,
            gtt_pte_base: self.gtt_pte_base.map(mmio32),
            gmadr_base: self.gmadr_base,
            gmadr_size: self.gmadr_size,
            stolen_base: self.stolen_base,
            stolen_size: self.stolen_size,
            gtt_size: self.gtt_size,
            gcfgc: self.gcfgc,
        }
    }
}

/// Program the physical base of the hardware GTT page table.
///
/// libgfxinit never touches `PGETBL_CTL`; coreboot programs it per chipset
/// immediately before calling libgfxinit, and the display engine cannot
/// resolve framebuffer addresses until the enable bit is set. `flags` carries
/// the per-generation GTT size encoding.
pub fn program_gtt_base(gtt_mmio_base: u64, gtt_base: u32, flags: u32) {
    let flush = (gtt_mmio_base + u64::from(GFX_FLSH_CNTL)) as *mut u32;
    let pgetbl = (gtt_mmio_base + u64::from(PGETBL_CTL)) as *mut u32;
    // SAFETY: the caller has programmed and enabled the GTTMMADR BAR, and both
    // offsets are fixed registers inside that window.
    unsafe {
        fstart_core::mmio::write32(flush, 0);
        fstart_core::mmio::write32(pgetbl, gtt_base | flags);
        fstart_core::mmio::write32(flush, 0);
    }
}

/// Clear a GTT page-table aperture through its MMIO-visible window.
///
/// Stale page tables left by a previous firmware would otherwise alias random
/// physical pages into the graphics address space.
pub fn clear_gtt_table(pte_base: u64, size: u32) {
    let entries = (size / core::mem::size_of::<u32>() as u32) as usize;
    for index in 0..entries {
        let pte = (pte_base as usize + index * core::mem::size_of::<u32>()) as *mut u32;
        // SAFETY: the caller has programmed the GTT aperture this range covers.
        unsafe { fstart_core::mmio::write32(pte, 0) };
    }
}

/// IGD display state owned by a northbridge driver.
#[derive(Debug, Clone, Copy, Default)]
pub struct IgdDisplay {
    info: Option<FramebufferInfo>,
}

impl IgdDisplay {
    /// Create an uninitialized display state.
    #[must_use]
    pub const fn new() -> Self {
        Self { info: None }
    }

    /// Framebuffer programmed by [`Self::initialize`], if it succeeded.
    #[must_use]
    pub const fn framebuffer_info(&self) -> Option<FramebufferInfo> {
        self.info
    }

    /// Program the display engine and record the resulting framebuffer.
    ///
    /// Best effort, like coreboot's `gma_func0_init`: a board with no display
    /// policy, or one whose panel or monitor cannot be brought up, still boots.
    /// Returns whether a framebuffer was programmed. The chipset prerequisites
    /// (BAR programming, GDRST reset, OpRegion, GTT base) are the caller's job.
    pub fn initialize(
        &mut self,
        cpu: Cpu,
        policy: Option<&IgdDisplayPolicy>,
        addresses: &IgdAddresses,
        vbt: Option<&[u8]>,
    ) -> bool {
        #[cfg(feature = "display")]
        match self.modeset(cpu, policy, addresses, vbt) {
            Ok(programmed) => programmed,
            Err(err) => {
                fstart_log::error!("intel-igd: display initialization failed: {}", err.as_str());
                false
            }
        }
        #[cfg(not(feature = "display"))]
        {
            let _ = (cpu, policy, addresses, vbt);
            false
        }
    }

    #[cfg(feature = "display")]
    #[cfg(feature = "display")]
    fn modeset(
        &mut self,
        cpu: Cpu,
        policy: Option<&IgdDisplayPolicy>,
        addresses: &IgdAddresses,
        vbt: Option<&[u8]>,
    ) -> Result<bool, fstart_intel_gma::GmaError> {
        let Some(policy) = policy else {
            return Ok(false);
        };
        let config = GmaInitConfig {
            cpu,
            outputs: policy.outputs,
            framebuffer: policy.framebuffer,
            vbt,
        };
        match fstart_intel_gma::init(&addresses.to_gma_resources(), &config) {
            Ok(GmaInitResult { framebuffer }) => {
                fstart_log::info!(
                    "intel-igd: {}x{} framebuffer at {:#x}",
                    framebuffer.width,
                    framebuffer.height,
                    framebuffer.base_addr
                );
                self.info = Some(framebuffer);
                Ok(true)
            }
            Err(err) => Err(err),
        }
    }
}

impl Framebuffer for IgdDisplay {
    fn info(&self) -> FramebufferInfo {
        self.info.unwrap_or(NO_FRAMEBUFFER)
    }
}
