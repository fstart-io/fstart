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
use tock_registers::interfaces::Writeable;
use tock_registers::registers::ReadWrite as MmioReadWrite;
use tock_registers::{register_bitfields, register_structs};
use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// `PGETBL_CTL`: physical base and flags of the GTT page table.
pub const PGETBL_CTL: u32 = 0x02020;
/// `GFX_FLSH_CNTL`: flush register for GTT writes.
pub const GFX_FLSH_CNTL: u32 = 0x02170;
/// `PGETBL_CTL` bit that enables GTT address translation.
pub const PGETBL_ENABLED: u32 = 1;

register_bitfields![u32,
    PgetblCtl [
        ENABLE OFFSET(0) NUMBITS(1) [],
        TABLE_SIZE OFFSET(1) NUMBITS(1) []
    ]
];

register_structs! {
    IgdGttRegisters {
        (0x0000 => _reserved0: [u8; 0x2020]),
        (0x2020 => pgetbl_ctl: MmioReadWrite<u32, PgetblCtl::Register>),
        (0x2024 => _reserved1: [u8; 0x2170 - 0x2024]),
        (0x2170 => gfx_flush: MmioReadWrite<u32>),
        (0x2174 => @END),
    }
}

/// PCI vendor ID shared by every Intel IGD policy check in this crate.
///
/// Kept here — next to the shared display bring-up — so the gm965, i945,
/// and Pineview drivers match against one constant instead of each
/// declaring their own.
pub const INTEL_VENDOR_ID: u16 = 0x8086;

/// Pure policy: does `(vendor, device)` match one of the expected IGD IDs?
///
/// No hardware access. Each chipset driver keeps a thin `const fn` wrapper
/// pinning its own ID table (unit-tested there); this owns the single copy
/// of the vendor check and the table scan so a misconfigured board fails
/// closed to headless.
#[must_use]
pub const fn igd_id_matches(vendor: u16, device: u16, expected: &[u16]) -> bool {
    if vendor != INTEL_VENDOR_ID {
        return false;
    }
    let mut i = 0;
    while i < expected.len() {
        if device == expected[i] {
            return true;
        }
        i += 1;
    }
    false
}

/// Decode the GMCH Multi Size Aperture Control register.
///
/// These generations expose either a 128 MiB or 256 MiB CPU aperture. The
/// northbridge programs MSAC before display setup, so later code reads the
/// resulting hardware state instead of repeating the size in board metadata.
#[must_use]
pub const fn gmadr_size_from_msac(msac: u8) -> u32 {
    match msac & 0x3 {
        0x0 => 128 * 1024 * 1024,
        0x2 => 256 * 1024 * 1024,
        _ => 0,
    }
}

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
///
/// This is `sizeof(igd_opregion_t)` in coreboot, and the value the header's
/// size field reports even when a VBT extension follows it.
pub const OPREGION_BASE_SIZE: usize = 8 * 1024;
/// VBT mailbox inside the base region, coreboot's `opregion->vbt.gvd1`.
const OPREGION_VBT_INLINE_OFFSET: usize = 0x400;
/// Largest VBT that fits the inline mailbox.
const OPREGION_VBT_INLINE_SIZE: usize = 6 * 1024;
/// VBT extension address, relative to the OpRegion base in version 2.1+.
const OPREGION_VBT_EXT_OFFSET: usize = OPREGION_BASE_SIZE;
/// Offset of the VBT core block's BIOS build stamp.
///
/// coreboot copies `vbt->coreblock_biosbuild` into the header's
/// `vbios_version` field.
const VBT_BIOS_BUILD_OFFSET: usize = 79;
/// Length of the VBT BIOS build stamp.
const VBT_BIOS_BUILD_SIZE: usize = 4;

/// Allocation size for an OpRegion carrying `vbt_len` bytes of VBT.
///
/// coreboot sizes its OpRegion as the base region plus the VBT aligned to a
/// 512-byte boundary, even when the VBT fits the inline mailbox.
#[must_use]
pub const fn opregion_size(vbt_len: usize) -> usize {
    OPREGION_BASE_SIZE + vbt_len.div_ceil(512) * 512
}
/// Backlight levels advertised in mailbox 3, scaled to 0xffff.
const OPREGION_BRIGHTNESS_LEVELS: [u16; 11] = [
    0x0000, 0x0a19, 0x1433, 0x1e4c, 0x2866, 0x327f, 0x3c99, 0x46b2, 0x50cc, 0x5ae5, 0x64ff,
];

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
struct OpRegionHeader {
    signature: [u8; 16],
    size_kib: U32<LE>,
    _reserved0: [u8; 2],
    version: u8,
    revision: u8,
    _reserved1: [u8; 32],
    vbios_version: [u8; 4],
    _reserved2: [u8; 28],
    supported_mailboxes: U32<LE>,
    _reserved3: [u8; 164],
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
struct OpRegionMailbox1 {
    _reserved0: [u8; 172],
    driver_ready: U32<LE>,
    _reserved1: [u8; 80],
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
struct OpRegionMailbox3 {
    _reserved0: [u8; 16],
    supported_events: U32<LE>,
    requested_brightness: U32<LE>,
    current_brightness: U32<LE>,
    brightness_levels: [U16<LE>; 11],
    _reserved1: [u8; 136],
    vbt_address: U64<LE>,
    vbt_size: U32<LE>,
    _reserved2: [u8; 58],
}

/// Fill the OpRegion body that the OS reads after it is published via ASLS.
///
/// This is the layout coreboot's `intel_gma_init_igd_opregion` produces: the
/// header, the display and backlight mailboxes, and the VBT either inline or in
/// the extension area when it is larger than the inline mailbox. The caller
/// publishes the buffer and selects the chipset's SCI register, which differs
/// between the GMCH parts and the Atom platforms.
pub fn build_opregion(buffer: &mut [u8], vbt: &[u8]) {
    let size = opregion_size(vbt.len());
    buffer[..size].fill(0);
    {
        let header = OpRegionHeader::mut_from_bytes(&mut buffer[..0x100])
            .expect("OpRegion header has its wire size");
        header.signature = *b"IntelGraphicsMem";
        header.size_kib.set((OPREGION_BASE_SIZE / 1024) as u32);
        header.version = 1;
        header.revision = 2;
        if vbt.len() >= VBT_BIOS_BUILD_OFFSET + VBT_BIOS_BUILD_SIZE {
            // Video BIOS build stamp, which the OS reports in its OpRegion dump.
            header.vbios_version.copy_from_slice(
                &vbt[VBT_BIOS_BUILD_OFFSET..VBT_BIOS_BUILD_OFFSET + VBT_BIOS_BUILD_SIZE],
            );
        }
        // Supported mailboxes: ACPI, ASLE and the extended ASLE mailbox.
        header
            .supported_mailboxes
            .set((1 << 0) | (1 << 2) | (1 << 3) | (1 << 4));
    }
    OpRegionMailbox1::mut_from_bytes(&mut buffer[0x100..0x200])
        .expect("OpRegion mailbox 1 has its wire size")
        .driver_ready
        .set(1);
    {
        let mailbox = OpRegionMailbox3::mut_from_bytes(&mut buffer[0x300..0x400])
            .expect("OpRegion mailbox 3 has its wire size");
        mailbox.supported_events.set(0xff);
        mailbox.requested_brightness.set((1 << 31) | 6);
        mailbox.current_brightness.set((1 << 31) | 0x64);
        for (entry, level) in mailbox
            .brightness_levels
            .iter_mut()
            .zip(OPREGION_BRIGHTNESS_LEVELS)
        {
            entry.set(0x8000 | level);
        }
    }

    if vbt.len() <= OPREGION_VBT_INLINE_SIZE {
        buffer[OPREGION_VBT_INLINE_OFFSET..OPREGION_VBT_INLINE_OFFSET + vbt.len()]
            .copy_from_slice(vbt);
        return;
    }

    // Larger VBTs live in the extension area just past the base region, which
    // mailbox 3 points at with a base-relative address.
    buffer[OPREGION_VBT_EXT_OFFSET..OPREGION_VBT_EXT_OFFSET + vbt.len()].copy_from_slice(vbt);
    let mailbox = OpRegionMailbox3::mut_from_bytes(&mut buffer[0x300..0x400])
        .expect("OpRegion mailbox 3 has its wire size");
    mailbox.vbt_address.set(OPREGION_BASE_SIZE as u64);
    mailbox.vbt_size.set(vbt.len().div_ceil(512) as u32 * 512);
}

/// VBT signature, `$VBT`.
pub const VBT_SIGNATURE: u32 = 0x5442_5624;
#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
struct VbtHeaderPrefix {
    signature: [u8; 20],
    version: U16<LE>,
    header_size: U16<LE>,
    size: U16<LE>,
    checksum: u8,
    _reserved: u8,
    bdb_offset: U32<LE>,
}
/// Legacy option-ROM window probed when no VBT is staged.
const LEGACY_VBT_WINDOW: usize = 128 * 1024;
/// Scan stride through the legacy window.
const LEGACY_VBT_STRIDE: usize = 16;

/// Where a board's VBT comes from.
///
/// A verified FFS asset is authoritative: when one is named, a verification
/// failure must not silently fall back to unverified memory. Otherwise raw
/// firmware-staged bytes are used, then the legacy VBIOS window is scanned.
#[derive(Debug, Clone, Copy)]
pub struct VbtSource {
    /// Board-relative VBT file stored as a compressed FFS data file.
    pub file: Option<&'static str>,
    /// Raw VBT blob staged at a physical address by board firmware.
    pub staged: Option<(u64, u32)>,
    /// Legacy option-ROM window to scan for a `$VBT` signature.
    pub legacy_probe: Option<u64>,
}

impl VbtSource {
    /// No VBT at all; the OpRegion is not published.
    pub const NONE: Self = Self {
        file: None,
        staged: None,
        legacy_probe: None,
    };
    /// Scan the PC legacy VBIOS window at `0xc0000`.
    pub const LEGACY: Self = Self {
        legacy_probe: Some(0x000C_0000),
        ..Self::NONE
    };

    /// A verified FFS asset.
    #[must_use]
    pub const fn ffs(file: &'static str) -> Self {
        Self {
            file: Some(file),
            ..Self::NONE
        }
    }
}

/// VBT bytes: borrowed from a fixed window, or copied out of a verified asset.
pub type Vbt = alloc::borrow::Cow<'static, [u8]>;

/// Validate a VBT header and return its declared length.
#[must_use]
pub fn vbt_len(bytes: &[u8]) -> Option<usize> {
    let (header, _) = VbtHeaderPrefix::ref_from_prefix(bytes).ok()?;
    if !header.signature.starts_with(&VBT_SIGNATURE.to_le_bytes()) {
        return None;
    }
    let size = usize::from(header.size.get());
    (size != 0 && size <= bytes.len()).then_some(size)
}

/// Read a VBT from a raw physical address staged by board firmware.
fn staged_vbt(staged: Option<(u64, u32)>) -> Option<&'static [u8]> {
    let (address, size) = staged?;
    if size == 0 {
        return None;
    }
    // SAFETY: board config promises this physical address holds a raw VBT blob.
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, size as usize) };
    vbt_len(bytes).map(|len| &bytes[..len])
}

/// Scan the legacy VBIOS window for a VBT.
fn legacy_vbt(probe_base: Option<u64>) -> Option<&'static [u8]> {
    let base = probe_base? as usize;
    // SAFETY: the legacy option-ROM window is readable on PC-compatible x86.
    let rom = unsafe { core::slice::from_raw_parts(base as *const u8, LEGACY_VBT_WINDOW) };
    (0..rom.len() - 4)
        .step_by(LEGACY_VBT_STRIDE)
        .find_map(|offset| vbt_len(&rom[offset..]).map(|size| &rom[offset..offset + size]))
}

/// Locate the board's VBT per [`VbtSource`] policy.
#[must_use]
pub fn locate_vbt(source: &VbtSource) -> Option<Vbt> {
    if let Some(file_name) = source.file {
        let bytes = fstart_core::services::ffs_context::read_verified_asset(file_name)?;
        let len = vbt_len(bytes)?;
        return Some(Vbt::Owned(bytes[..len].to_vec()));
    }
    staged_vbt(source.staged)
        .or_else(|| legacy_vbt(source.legacy_probe))
        .map(Vbt::Borrowed)
}

/// IGD config-space register that arms the OpRegion software SCI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpRegionSci {
    /// GMCH parts: `SWSCI` at 0xe8.
    Swsci,
    /// Atom parts: combined `SWSMISCI` at 0xe0.
    Swsmisci,
}

impl OpRegionSci {
    const fn offset(self) -> u16 {
        match self {
            Self::Swsci => 0xe8,
            Self::Swsmisci => 0xe0,
        }
    }
}

/// `ASLS`: OpRegion physical address the OS reads.
const IGD_ASLS: u16 = 0xfc;

/// Allocate the OpRegion once on the mainstage heap.
///
/// A `static` buffer would land in every stage's `.bss`, including the
/// bootblock whose CAR is as small as 32 KiB on Pineview. The OS reads the
/// region through ASLS for the machine's lifetime, so it is never freed.
fn opregion_buf(size: usize) -> &'static mut [u8] {
    let layout = core::alloc::Layout::from_size_align(size, 4096).expect("opregion layout");
    // SAFETY: non-zero-sized layout; the allocation is intentionally leaked.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "IGD opregion allocation failed");
    // SAFETY: fresh, exclusively owned, zeroed allocation of `size` bytes.
    unsafe { core::slice::from_raw_parts_mut(ptr, size) }
}

/// Locate the VBT, build the OpRegion around it and publish it through
/// `ASLS` (coreboot's `intel_gma_init_igd_opregion`). Returns the VBT so the
/// modeset can consume the same bytes.
///
/// Best effort: a missing VBT logs and leaves the OS without an OpRegion.
/// Callers check that the IGD function is present first.
pub fn publish_opregion(
    igd: &fstart_pci::ecam::EcamDevice,
    sci: OpRegionSci,
    source: &VbtSource,
) -> Option<Vbt> {
    let Some(vbt) = locate_vbt(source) else {
        fstart_log::error!("intel-igd: no valid VBT found for the OpRegion");
        return None;
    };
    publish_opregion_bytes(igd, sci, &vbt);
    Some(vbt)
}

fn publish_opregion_bytes(igd: &fstart_pci::ecam::EcamDevice, sci: OpRegionSci, vbt: &[u8]) {
    let opregion = opregion_buf(opregion_size(vbt.len()));
    build_opregion(opregion, vbt);

    igd.write32(IGD_ASLS, opregion.as_ptr() as u32);
    let sci_reg = sci.offset();
    igd.write16(sci_reg, (igd.read16(sci_reg) & !1) | (1 << 15));
    fstart_log::info!(
        "intel-igd: OpRegion at {:#x}, VBT {} bytes",
        opregion.as_ptr() as usize,
        vbt.len()
    );
}

const IGD_BAR0_GTTMMADR: u16 = 0x10;
const IGD_BAR2_GMADR: u16 = 0x18;
const IGD_BAR3_GTTADR: u16 = 0x1c;

/// IGD windows as PCI enumeration assigned them.
///
/// The allocator lays these out among every other device; drivers consume
/// its assignment instead of re-programming fixed addresses, which on
/// Pineview moved the GMCH register block away from the strapped window and
/// on every chipset can alias a neighbour's BAR.
#[derive(Debug, Clone, Copy)]
pub struct IgdBars {
    /// GTTMMADR (BAR0): the display MMIO window.
    pub gtt_mmio: u64,
    /// GMADR (BAR2): the CPU-visible graphics aperture.
    pub gmadr: u64,
    /// GTTADR (BAR3): the MMIO-visible GTT page table on chipsets that expose
    /// one (Pineview); `None` where the GTT lives inside GTTMMADR.
    pub gtt_pte: Option<u64>,
}

/// Read the IGD BARs after resource allocation.
///
/// `gtt_pte_bar` selects chipsets with a separate GTT page-table BAR. Logs
/// and returns `None` when a required window was left unassigned; display
/// bring-up is then skipped rather than guessing an address.
#[must_use]
pub fn assigned_bars(igd: &fstart_pci::ecam::EcamDevice, gtt_pte_bar: bool) -> Option<IgdBars> {
    let bars = IgdBars {
        gtt_mmio: igd.memory_bar(IGD_BAR0_GTTMMADR)?,
        gmadr: igd.memory_bar(IGD_BAR2_GMADR)?,
        gtt_pte: if gtt_pte_bar {
            Some(igd.memory_bar(IGD_BAR3_GTTADR)?)
        } else {
            None
        },
    };
    fstart_log::info!(
        "intel-igd: GTTMMADR={:#x} GMADR={:#x} GTTADR={:?}",
        bars.gtt_mmio,
        bars.gmadr,
        bars.gtt_pte
    );
    Some(bars)
}

/// Read a display MMIO register relative to GTTMMADR.
#[must_use]
pub fn mmio_read32(gtt_mmio: u64, offset: u32) -> u32 {
    // SAFETY: GTTMMADR was assigned by enumeration and memory decoding is
    // enabled before any display register access.
    unsafe { fstart_core::mmio::read32((gtt_mmio + u64::from(offset)) as *const u32) }
}

/// Write a display MMIO register relative to GTTMMADR.
pub fn mmio_write32(gtt_mmio: u64, offset: u32, value: u32) {
    // SAFETY: see `mmio_read32`.
    unsafe { fstart_core::mmio::write32((gtt_mmio + u64::from(offset)) as *mut u32, value) }
}

/// Program the physical base of the hardware GTT page table.
///
/// libgfxinit never touches `PGETBL_CTL`; coreboot programs it per chipset
/// immediately before calling libgfxinit, and the display engine cannot
/// resolve framebuffer addresses until the enable bit is set. `flags` carries
/// the per-generation GTT size encoding.
pub fn program_gtt_base(gtt_mmio_base: u64, gtt_base: u32, flags: u32) {
    // SAFETY: GTTMMADR was assigned by PCI enumeration and memory decoding is
    // enabled before display setup. The typed view covers only documented
    // registers inside that BAR.
    let registers = unsafe { &*(gtt_mmio_base as *const IgdGttRegisters) };
    registers.gfx_flush.set(0);
    registers.pgetbl_ctl.set(gtt_base | flags);
    registers.gfx_flush.set(0);
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
        // Report what the DDC/GMBUS/EDID path yields before the modeset: on
        // Pineview the GMBUS unit's clock gate must be un-gated for the read to
        // complete at all, and a failed read silently degrades to the fixed
        // board mode.
        match fstart_intel_gma::edid_mode(&addresses.to_gma_resources(), &config) {
            Ok(mode) => fstart_log::info!(
                "intel-igd: EDID preferred mode {}x{}@{}kHz",
                mode.hdisplay,
                mode.vdisplay,
                mode.pixel_clock_khz
            ),
            Err(_) => fstart_log::warn!("intel-igd: no EDID mode from DDC (read failed)"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_opregion_layout_matches_the_wire_offsets() {
        let mut vbt = alloc::vec![0u8; OPREGION_VBT_INLINE_SIZE + 1];
        vbt[VBT_BIOS_BUILD_OFFSET..VBT_BIOS_BUILD_OFFSET + VBT_BIOS_BUILD_SIZE]
            .copy_from_slice(b"TEST");
        let mut opregion = alloc::vec![0u8; opregion_size(vbt.len())];
        build_opregion(&mut opregion, &vbt);

        let header = OpRegionHeader::ref_from_bytes(&opregion[..0x100]).unwrap();
        assert_eq!(&header.signature, b"IntelGraphicsMem");
        assert_eq!(header.size_kib.get(), 8);
        assert_eq!(&header.vbios_version, b"TEST");
        assert_eq!(header.supported_mailboxes.get(), 0x1d);

        let mailbox = OpRegionMailbox3::ref_from_bytes(&opregion[0x300..0x400]).unwrap();
        assert_eq!(mailbox.vbt_address.get(), OPREGION_BASE_SIZE as u64);
        assert_eq!(mailbox.vbt_size.get(), 6656);
        assert_eq!(
            &opregion[OPREGION_VBT_EXT_OFFSET..OPREGION_VBT_EXT_OFFSET + vbt.len()],
            vbt.as_slice()
        );
    }

    #[test]
    fn typed_vbt_prefix_validates_signature_and_length() {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&VBT_SIGNATURE.to_le_bytes());
        bytes[24..26].copy_from_slice(&32u16.to_le_bytes());
        assert_eq!(vbt_len(&bytes), Some(32));
        bytes[24..26].copy_from_slice(&33u16.to_le_bytes());
        assert_eq!(vbt_len(&bytes), None);
    }
}
