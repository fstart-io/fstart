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

/// OpRegion base region: the part Linux reads through the ASLS register.
pub const OPREGION_BASE_SIZE: usize = 8 * 1024;
/// OpRegion allocation, including the VBT extension area past the base region.
pub const OPREGION_TOTAL_SIZE: usize = 16 * 1024;
/// VBT mailbox inside the base region.
const OPREGION_VBT_INLINE_OFFSET: usize = 0x400;
/// Largest VBT that fits the inline mailbox.
const OPREGION_VBT_INLINE_SIZE: usize = 6 * 1024;
/// VBT extension area, referenced from mailbox 3.
const OPREGION_VBT_EXT_OFFSET: usize = OPREGION_BASE_SIZE;
/// Backlight levels advertised in mailbox 3, scaled to 0xffff.
const OPREGION_BRIGHTNESS_LEVELS: [u16; 11] = [
    0x0000, 0x0a19, 0x1433, 0x1e4c, 0x2866, 0x327f, 0x3c99, 0x46b2, 0x50cc, 0x5ae5, 0x64ff,
];

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Fill the OpRegion body that the OS reads after it is published via ASLS.
///
/// This is the layout coreboot's `intel_gma_init_igd_opregion` produces: the
/// header, the display and backlight mailboxes, and the VBT either inline or in
/// the extension area when it is larger than the inline mailbox. The caller
/// publishes the buffer and selects the chipset's SCI register, which differs
/// between the GMCH parts and the Atom platforms.
pub fn build_opregion(buffer: &mut [u8], vbt: &[u8]) {
    buffer[..OPREGION_TOTAL_SIZE].fill(0);
    buffer[0..16].copy_from_slice(b"IntelGraphicsMem");
    write_u32(buffer, 16, (OPREGION_BASE_SIZE / 1024) as u32);
    buffer[22] = 1;
    buffer[23] = 2;
    if vbt.len() >= 82 {
        // Panel type and backlight fields the OS reads from the VBT header.
        buffer[56..60].copy_from_slice(&vbt[78..82]);
    }
    // Supported mailboxes: ACPI, ASLE and the extended ASLE mailbox.
    write_u32(buffer, 88, (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4));
    write_u32(buffer, 0x100 + 172, 1);
    write_u32(buffer, 0x300 + 16, 0xff);
    write_u32(buffer, 0x300 + 20, (1 << 31) | 6);
    write_u32(buffer, 0x300 + 24, (1 << 31) | 0x64);
    for (index, level) in OPREGION_BRIGHTNESS_LEVELS.iter().copied().enumerate() {
        write_u16(buffer, 0x300 + 28 + index * 2, 0x8000 | level);
    }

    if vbt.len() <= OPREGION_VBT_INLINE_SIZE {
        buffer[OPREGION_VBT_INLINE_OFFSET..OPREGION_VBT_INLINE_OFFSET + vbt.len()]
            .copy_from_slice(vbt);
        return;
    }

    // Larger VBTs live in the extension area, which mailbox 3 points at.
    let available = OPREGION_TOTAL_SIZE - OPREGION_VBT_EXT_OFFSET;
    let extension_size = ((vbt.len() + 511) & !511).min(available);
    let copy_len = vbt.len().min(extension_size);
    buffer[OPREGION_VBT_EXT_OFFSET..OPREGION_VBT_EXT_OFFSET + copy_len]
        .copy_from_slice(&vbt[..copy_len]);
    write_u64(buffer, 0x300 + 186, OPREGION_BASE_SIZE as u64);
    write_u32(buffer, 0x300 + 194, extension_size as u32);
}

/// VBT signature, `$VBT`.
pub const VBT_SIGNATURE: u32 = 0x5442_5624;
/// Offset of the total VBT length in the VBT header.
const VBT_LENGTH_OFFSET: usize = 24;
/// Minimum header length needed to read the VBT length.
const VBT_MIN_LEN: usize = 28;
/// Legacy option-ROM window probed when no VBT is staged.
const LEGACY_VBT_WINDOW: usize = 128 * 1024;
/// Scan stride through the legacy window.
const LEGACY_VBT_STRIDE: usize = 16;

/// VBT bytes: borrowed from a fixed window, or copied out of a verified asset.
pub enum VbtBytes<'a> {
    /// Points into firmware-owned memory.
    Borrowed(&'a [u8]),
    /// Copied out of a verified FFS asset.
    Owned(alloc::vec::Vec<u8>),
}

impl VbtBytes<'_> {
    /// The VBT bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes.as_slice(),
        }
    }
}

/// Validate a VBT header and return its declared length.
#[must_use]
pub fn vbt_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < VBT_MIN_LEN
        || u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != VBT_SIGNATURE
    {
        return None;
    }
    let size =
        u16::from_le_bytes([bytes[VBT_LENGTH_OFFSET], bytes[VBT_LENGTH_OFFSET + 1]]) as usize;
    (size != 0 && size <= bytes.len()).then_some(size)
}

/// Read a VBT from a raw physical address staged by board firmware.
fn configured_vbt(address: Option<u64>, size: u32) -> Option<&'static [u8]> {
    let address = address? as usize;
    let size = size as usize;
    if size == 0 {
        return None;
    }
    // SAFETY: board config promises this physical address holds a raw VBT blob.
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, size) };
    vbt_len(bytes).map(|len| &bytes[..len])
}

/// Scan the legacy VBIOS window for a VBT.
fn legacy_vbt(probe_base: Option<u64>) -> Option<&'static [u8]> {
    let base = probe_base? as usize;
    // SAFETY: the legacy option-ROM window is readable on PC-compatible x86.
    let rom = unsafe { core::slice::from_raw_parts(base as *const u8, LEGACY_VBT_WINDOW) };
    let mut offset = 0usize;
    while offset + 4 < rom.len() {
        if u32::from_le_bytes([
            rom[offset],
            rom[offset + 1],
            rom[offset + 2],
            rom[offset + 3],
        ]) == VBT_SIGNATURE
            && let Some(size) = vbt_len(&rom[offset..])
        {
            return Some(&rom[offset..offset + size]);
        }
        offset += LEGACY_VBT_STRIDE;
    }
    None
}

/// Locate the board's VBT.
///
/// A configured authenticated asset is authoritative: if the board names one,
/// a verification failure must not silently fall back to unverified memory.
/// Otherwise firmware-staged bytes are used, then the legacy VBIOS window.
#[must_use]
pub fn locate_vbt(
    vbt_file: Option<&'static str>,
    vbt_addr: Option<u64>,
    vbt_size: u32,
    legacy_vbt_probe: Option<u64>,
) -> Option<VbtBytes<'static>> {
    #[cfg(feature = "ffs-vbt")]
    if let Some(file_name) = vbt_file {
        let bytes = fstart_core::services::ffs_context::read_verified_asset(file_name)?;
        let len = vbt_len(bytes)?;
        return Some(VbtBytes::Owned(bytes[..len].to_vec()));
    }
    let _ = vbt_file;
    configured_vbt(vbt_addr, vbt_size)
        .or_else(|| legacy_vbt(legacy_vbt_probe))
        .map(VbtBytes::Borrowed)
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
