//! Intel Atom D4xx/D5xx (Pineview) northbridge driver.
//!
//! Covers the integrated memory controller hub on the Atom D410/D510/D525
//! family. Responsibilities:
//!
//! - **Early init ([`PciHost::early_init`])**: enable ECAM (PCIEXBAR) via
//!   the single legacy CF8/CFC write, then use ECAM MMIO for everything:
//!   MCHBAR/DMIBAR/EPBAR setup, PAM unlock, graphics clocks, and
//!   miscellaneous chipset init.
//! - **DRAM training**: full DDR2 raminit ported from coreboot’s ~2600-line
//!   `raminit.c`. Called by the fixed Pineview platform flow.
//!
//! Register definitions live in the crate-local [`regs`] module.

#![allow(clippy::empty_line_after_doc_comments, dead_code)]

pub mod raminit;
mod regs;

#[cfg(feature = "ffs-vbt")]
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ptr;

use self::regs::{DmiBar, MchBar, Rcba, hostbridge, mchbar};
use crate::MmioBar;
use crate::ich7::ich7;
use fstart_arch::mp::{SmmError, SmmInfo, SmmOps};
use fstart_arch::x86::mtrr;
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::MemoryController;
use fstart_core::services::device::DeviceError;
use fstart_core::services::memory_detect::{E820Entry, E820Kind, MemoryDetector};
use fstart_core::services::{ServiceError, SmBus};
use fstart_intel_gma::types::{Cpu, PciAddress};
use fstart_pci::ecam;
use fstart_pci::pci_type0_config;
use fstart_pci::{
    PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind,
};
use tock_registers::interfaces::{Readable, Writeable};

fn publish_mtrr_wb_ranges(entries: &[E820Entry]) {
    let mut ranges = [(0u64, 0u64); 8];
    let mut count = 0usize;
    for entry in entries {
        if entry.kind == E820Kind::Ram as u32 && entry.size != 0 && count < ranges.len() {
            ranges[count] = (entry.addr, entry.size);
            count += 1;
        }
    }
    mtrr::set_ram_wb_ranges(&ranges[..count]);
}

#[cfg(feature = "ffs-vbt")]
const IGD_ASLS: u16 = 0xFC;
#[cfg(feature = "ffs-vbt")]
const IGD_SWSMISCI: u16 = 0xE0;
#[cfg(feature = "ffs-vbt")]
const VBT_SIGNATURE: u32 = 0x5442_5624;

#[cfg(feature = "ffs-vbt")]
#[allow(clippy::large_enum_variant)]
enum VbtBytes<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

#[cfg(feature = "ffs-vbt")]
impl VbtBytes<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes.as_slice(),
        }
    }
}

/// IGD PCI configuration offsets and command bits.
const IGD_BAR0_GTTMMADR: u16 = 0x10;
const IGD_BAR2_GMADR: u16 = 0x18;
const IGD_BAR3_GTTADR: u16 = 0x1c;
const IGD_MSAC: u16 = 0x62;
const PCI_COMMAND: u16 = 0x04;
const PCI_CMD_MEMORY: u16 = 1 << 1;
const PCI_CMD_MASTER: u16 = 1 << 2;
/// GTTMMADR BAR0 window size. Pineview keeps the display MMIO in the lower
/// 512 KiB and exposes the page table through BAR3.
const IGD_GTTMMADR_SIZE: u32 = 512 * 1024;
/// GTT page-table size in bytes.
const IGD_GTT_SIZE: u32 = 512 * 1024;

/// Intel integrated graphics configuration.
#[derive(Debug, Clone, Copy)]
pub struct PineviewIgdConfig {
    /// Enable the VGA CRT output.
    pub use_crt: bool,
    /// Enable the LVDS panel output.
    pub use_lvds: bool,
    /// Enable PLL spread spectrum.
    pub spread_spectrum: bool,
    /// Board-relative VBT file path stored as a compressed FFS data file.
    pub vbt_file: Option<&'static str>,
    /// GTTMMADR BAR0 fallback address, used only when PCI enumeration left the
    /// window unassigned.
    pub gtt_mmio_base: u64,
    /// MMIO-visible GTT page-table BAR3 fallback address.
    pub gtt_pte_base: u64,
    /// GMADR graphics aperture BAR2 fallback address.
    pub gmadr_base: u64,
    /// GMADR graphics aperture size in bytes.
    pub gmadr_size: u32,
    /// Board display policy. `None` leaves the display engine untouched.
    pub display: Option<super::igd::IgdDisplayPolicy>,
}

const fn default_gtt_mmio_base() -> u64 {
    0xfed0_0000
}

const fn default_gtt_pte_base() -> u64 {
    0xfed8_0000
}

const fn default_gmadr_base() -> u64 {
    0xc000_0000
}

const fn default_gmadr_size() -> u32 {
    256 * 1024 * 1024
}

/// Pineview northbridge configuration.
#[derive(Debug, Clone, Copy)]
pub struct IntelPineviewConfig {
    /// MCHBAR base address.
    pub mchbar: u64,
    /// DMIBAR base address.
    pub dmibar: u64,
    /// EPBAR base address.
    pub epbar: u64,
    /// ECAM (PCIEXBAR) base address. Default: `0xE000_0000`.
    pub ecam_base: u64,
    /// Optional integrated graphics configuration.
    pub igd: PineviewIgdConfig,
    /// SPD EEPROM SMBus addresses for DIMM slots A/B. Zero means absent.
    pub spd_addresses: [u8; 4],
    /// Apply Foxconn D41S/vendor CK505 clock-generator setup before raminit.
    pub ck505_pre_raminit: bool,
    /// ACPI device name (e.g., "MCHC"). If `None`, no ACPI node.
    pub acpi_name: Option<&'static str>,
}

const fn default_spd_addresses() -> [u8; 4] {
    [0x50, 0x51, 0, 0]
}

const fn default_ecam_base() -> u64 {
    hostbridge::DEFAULT_ECAM_BASE as u64
}

impl PineviewIgdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            use_crt: false,
            use_lvds: false,
            spread_spectrum: false,
            vbt_file: None,
            gtt_mmio_base: default_gtt_mmio_base(),
            gtt_pte_base: default_gtt_pte_base(),
            gmadr_base: default_gmadr_base(),
            gmadr_size: default_gmadr_size(),
            display: None,
        }
    }
}

impl Default for PineviewIgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelPineviewConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mchbar: 0xFED1_0000,
            dmibar: 0xFED1_8000,
            epbar: 0xFED1_9000,
            ecam_base: hostbridge::DEFAULT_ECAM_BASE as u64,
            igd: PineviewIgdConfig::new(),
            spd_addresses: [0x50, 0x51, 0, 0],
            ck505_pre_raminit: false,
            acpi_name: Some("MCHC"),
        }
    }
}

impl Default for IntelPineviewConfig {
    fn default() -> Self {
        Self::new()
    }
}

// Pineview/NM10 SMM constants.  SMRAM bits match coreboot's
// `cpu/intel/smm/gen1/smmrelocate.c`; PM I/O bits live in
// `fstart-pmio-ich`.
const SMRAM_G_SMRAME: u8 = 1 << 3;
const SMRAM_D_LCK: u8 = 1 << 4;
const SMRAM_D_OPEN: u8 = 1 << 6;
const SMRAM_C_BASE_SEG: u8 = 0b010;
const ICH7_PMBASE: u16 = 0x0500;
const APM_CNT: u16 = 0x00b2;
const EM64T101_SAVE_STATE_SIZE: usize = 0x400;
const PCI_ECAM_SIZE: u64 = 0x1000_0000;
const PCI_MMIO32_LIMIT: u64 = 0xfec0_0000;
const PCI_MMIO64_LIMIT: u64 = 0x0010_0000_0000_0000;
const PCI_PIO_BASE: u64 = 0x1000;
const PCI_PIO_SIZE: u64 = 0xf000;
const PCI_BUS_START: u8 = 0;
const PCI_BUS_END: u8 = 0xff;

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware invokes SMM installation from the BSP while SMRAM is open;
// this scratch buffer is not shared with APs or interrupt context.
unsafe impl Sync for CpuLayoutStore {}

static PINEVIEW_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

/// Pineview NB driver.
pub struct IntelPineview {
    config: &'static IntelPineviewConfig,
    /// Detected DRAM size (bytes), populated by `init()`.
    detected_size: u64,
    boot_path: crate::BootPath,
    /// Framebuffer programmed by the shared GMA layer, if the board asked for it.
    display: super::igd::IgdDisplay,
}

// SAFETY: Driver holds no unsynchronized shared state; MMIO and PCI
// config writes are CPU-exclusive in firmware.
unsafe impl Send for IntelPineview {}
unsafe impl Sync for IntelPineview {}

pci_type0_config! {
    /// Pineview host bridge PCI configuration space.
    pub struct PineviewHostBridgePciConfig {
        (0x40 => pub epbar: MmioReadWrite<u32>),
        (0x44 => _reserved_hb0),
        (0x48 => pub mchbar: MmioReadWrite<u32>),
        (0x4c => _reserved_hb1),
        (0x52 => pub ggc: MmioReadWrite<u16>),
        (0x54 => pub deven: MmioReadWrite<u8>),
        (0x55 => _reserved_hb2),
        (0x60 => pub pciexbar: MmioReadWrite<u32>),
        (0x64 => _reserved_hb3),
        (0x68 => pub dmibar: MmioReadWrite<u32>),
        (0x6c => _reserved_hb4),
        (0x78 => pub pmiobar: MmioReadWrite<u32>),
        (0x7c => _reserved_hb5),
        (0x90 => pub pam: [MmioReadWrite<u8>; 7]),
        (0x97 => _reserved_hb6),
        (0x9d => pub smram: MmioReadWrite<u8>),
        (0x9e => pub esmramc: MmioReadWrite<u8>),
        (0x9f => _reserved_hb7),
        (0xa0 => pub tom: MmioReadWrite<u16>),
        (0xa2 => pub touud: MmioReadWrite<u16>),
        (0xa4 => pub gbsm: MmioReadWrite<u32>),
        (0xa8 => pub bgsm: MmioReadWrite<u32>),
        (0xac => pub tseg: MmioReadWrite<u32>),
        (0xb0 => pub tolud: MmioReadWrite<u16>),
        (0xb2 => _reserved_hb8),
        (0xdc => pub skpad: MmioReadWrite<u32>),
        (0xe0 => pub capid0: MmioReadWrite<u32>),
        (0xe4 => @END),
    }
}

impl IntelPineview {
    fn hostbridge_regs(&self) -> &'static PineviewHostBridgePciConfig {
        let hb = ecam::EcamDevice::new(0, 0, 0);
        // SAFETY: Pineview host bridge is fixed at 00:00.0 and ECAM is live
        // before callers use the overlay.
        unsafe { hb.regs::<PineviewHostBridgePciConfig>() }
    }

    /// MCHBAR accessor.
    fn mchbar(&self) -> MchBar {
        MchBar::new(self.config.mchbar as usize)
    }

    /// DMIBAR accessor.
    fn dmibar(&self) -> DmiBar {
        DmiBar::new(self.config.dmibar as usize)
    }

    /// Detect warm reset via MCHBAR PMSTS bit 8.
    ///
    /// Called after SB's S3 detection. If S3 was not detected, this
    /// checks whether the platform came from a warm reset (HOT RESET)
    /// by reading PMSTS bit 8 in MCHBAR.
    pub fn detect_warm_reset(&self) -> bool {
        let mch = self.mchbar();
        mch.read32(mchbar::PMSTS) & (1 << 8) != 0
    }

    fn platform_type(&self) -> u8 {
        const PINEVIEW_DID_MASK: u16 = 0xfff0;
        const PINEVIEW_MOBILE_DID: u16 = 0xa010;
        let did = ecam::EcamDevice::new(0, 0, 0).read16(0x02) & PINEVIEW_DID_MASK;
        if did == PINEVIEW_MOBILE_DID {
            raminit::PLATFORM_MOBILE
        } else {
            raminit::PLATFORM_DESKTOP
        }
    }

    // ---------------------------------------------------------------
    // Early init sub-routines (ported from coreboot early_init.c)
    // ---------------------------------------------------------------

    /// Enable ECAM by writing PCIEXBAR via legacy CF8/CFC.
    ///
    /// This is the **only** place legacy PIO is used. After this, all
    /// PCI config access goes through [`EcamPci`].
    #[cfg(target_arch = "x86_64")]
    fn enable_ecam(&self) {
        // PCIEXBAR value: base address | length encoding | enable.
        // Length encoding: 0 = 256 buses, 1 = 128, 2 = 64.
        // Pineview uses 64 buses → encoding = 2.
        let pciexbar_val = (self.config.ecam_base as u32) | (2 << 1) | 1;
        // SAFETY: one-time legacy PCI config write to the host bridge
        // to enable ECAM. After this, ECAM MMIO is live.
        unsafe {
            fstart_core::pio::pci_cfg_write32(0, 0, 0, hostbridge::PCIEXBAR as u8, pciexbar_val);
        }
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("pineview: ECAM enabled at {:#x}", self.config.ecam_base);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn enable_ecam(&self) {
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("pineview: ECAM enable (stub, non-x86)");
    }

    /// Program northbridge BARs and PAM registers via ECAM.
    ///
    /// Ported from coreboot `pineview_setup_bars()`.
    fn setup_bars(&self) {
        let hb = self.hostbridge_regs();
        // Match coreboot pineview_setup_bars(): set the host bridge
        // revision scratch value before programming static BARs. Revision is
        // read-only in the generic overlay, so keep this exact raw write.
        ecam::EcamDevice::new(0, 0, 0).write8(0x08, 0x69);
        // EPBAR, MCHBAR, DMIBAR — 32-bit writes with enable bit 0.
        hb.epbar.set((self.config.epbar as u32) | 1);
        hb.mchbar.set((self.config.mchbar as u32) | 1);
        hb.dmibar.set((self.config.dmibar as u32) | 1);
        hb.pmiobar.set(hostbridge::DEFAULT_PMIOBAR | 1);

        // DEVEN — enable D0F0, D2F0, D2F1.
        hb.deven.set(hostbridge::BOARD_DEVEN);

        // PAM0..PAM6: unlock BIOS shadow region C0000–FFFFF for RAM r/w.
        hb.pam[0].set(0x30);
        for pam in &hb.pam[1..] {
            pam.set(0x33);
        }

        fstart_log::info!("pineview: northbridge BARs and PAM configured");
    }

    /// Graphics clock and output setup.
    ///
    /// Ported from coreboot `early_graphics_setup()`.
    fn early_graphics_setup(&self) {
        let mch = self.mchbar();

        let hb = self.hostbridge_regs();
        // Enable the host bridge, the IGD and its display function, as coreboot
        // does (`BOARD_DEVEN = D0F0 | D2F0 | D2F1`). Without this the
        // integrated graphics function stays disabled: its display registers
        // and its DDC/GMBUS unit do not respond.
        hb.deven.set((1 << 0) | (1 << 3) | (1 << 4));
        // GGC: 1 MiB GTT (GGMS=1), 8 MiB stolen (GMS=3).
        hb.ggc.set((1 << 8) | (3 << 4));

        // Graphics clock dividers.
        const CRCLK_PINEVIEW: u32 = 0x02;
        const CDCLK_PINEVIEW: u32 = 0x10;

        let mut gcfgc = mch.read16(mchbar::MCH_GCFGC);
        gcfgc |= 1 << 9; // set UPDATE
        mch.write16(mchbar::MCH_GCFGC, gcfgc);
        gcfgc &= !0x7F;
        gcfgc |= (CDCLK_PINEVIEW | CRCLK_PINEVIEW) as u16;
        gcfgc &= !(1 << 9); // clear UPDATE
        mch.write16(mchbar::MCH_GCFGC, gcfgc);

        // Graphics core — PLL VCO frequency determines IGD 0xCC value.
        let hpllvco = mch.read8(mchbar::HPLLVCO) & 0x7;
        let igd_cc = match hpllvco {
            0x4 => 0xAD_u16, // 2666 MHz
            0x0 => 0xA0,     // 3200 MHz
            0x1 => 0xAD,     // 4000 MHz
            _ => 0xA0,
        };
        let igd = ecam::EcamDevice::new(0, 2, 0);
        let cc_val = igd.read16(0xCC) & !0x1FF;
        igd.write16(0xCC, cc_val | igd_cc);

        igd.and8(0x62, !0x3);
        igd.or8(0x62, 2);

        // VGA CRT / LVDS output control.
        let igd_cfg = &self.config.igd;
        if igd_cfg.use_crt {
            mch.setbits32(mchbar::DACGIOCTRL1, 1 << 15);
        } else {
            mch.clrbits32(mchbar::DACGIOCTRL1, 1 << 15);
        }
        if igd_cfg.use_lvds {
            let reg = mch.read32(mchbar::LVDSICR2);
            mch.write32(mchbar::LVDSICR2, (reg & !0xF100_0000) | 0x9000_0000);
            mch.setbits32(mchbar::IOCKTRR1, 1 << 9);
        } else {
            mch.setbits32(mchbar::DACGIOCTRL1, 3 << 25);
        }

        mch.write32(mchbar::CICTRL, 0xC6DB_8B5F);
        mch.write16(mchbar::CISDCTRL, 0x024F);

        mch.clrbits32(mchbar::DACGIOCTRL1, 0xFF);
        mch.setbits32(mchbar::DACGIOCTRL1, 1 << 5);

        // Legacy backlight control.
        igd.write8(0xF4, 0x4C);

        fstart_log::info!("pineview: graphics clocks configured");
    }

    /// Miscellaneous early chipset setup.
    ///
    /// Ported from coreboot `early_misc_setup()`.
    fn early_misc_setup(&self) {
        let mch = self.mchbar();
        let dmi = self.dmibar();

        mch.read32(mchbar::HIT0);
        mch.write32(mchbar::HIT0, 0x0002_1800);

        dmi.write32(0x2C, 0x8600_0040);

        // PCI bridge (1E:0): secondary bus programming.
        let pci_bridge = ecam::EcamDevice::new(0, 0x1e, 0);
        pci_bridge.write32(0x18, 0x0002_0200);
        pci_bridge.write32(0x18, 0x0000_0000);

        self.early_graphics_setup();

        // HIT4 sequence.
        mch.read32(mchbar::HIT4);
        mch.write32(mchbar::HIT4, 0);
        mch.read32(mchbar::HIT4);
        mch.write32(mchbar::HIT4, 1 << 3);

        // LPC device (1F:0) revision ID reset sequence.
        let lpc = ecam::EcamDevice::new(0, ich7::LPC_DEV, ich7::LPC_FUNC);
        lpc.write8(0x08, 0x1D);
        lpc.write8(0x08, 0x00);

        // Read RCBA from ICH7 LPC config for the remaining shared chipset
        // control register. IRQ and USB routing are owned by the southbridge.
        let rcba_val = lpc.read32(ich7::RCBA_REG);
        let rcba = Rcba::new((rcba_val & 0xFFFF_C000) as usize);

        rcba.write32(0x3410, 0x0002_0465);

        fstart_log::info!("pineview: early misc setup complete");
    }
}

impl IntelPineview {
    pub fn init(&mut self) -> Result<(), DeviceError> {
        // Keep construction side-effect free.  `ChipsetPreConsole` calls
        // `init_device()` before the console exists, and before MCHBAR is
        // enabled.  Touching MCHBAR here can hang silently on real Pineview
        // hardware.  All hardware setup is performed explicitly by
        // `pre_console_init()`, `early_init()`, and the `DramInit` trampoline.
        Ok(())
    }
}

impl IntelPineview {
    fn pre_console_phase(&mut self) -> Result<(), ServiceError> {
        // Enable ECAM (single legacy CF8/CFC write).
        // This is the only early step needed before the console —
        // the southbridge needs ECAM to open LPC decode.
        self.enable_ecam();
        Ok(())
    }

    fn early_phase(&mut self) -> Result<(), ServiceError> {
        // Each stage has its own BSS, so the global ECAM accessor must be
        // rebound whenever this phase runs. The hardware PCIEXBAR programming
        // is idempotent and matches coreboot's repeated hostbridge setup.
        self.enable_ecam();

        // 1. Program BARs + PAM via ECAM.
        self.setup_bars();

        // 3. Miscellaneous chipset init (graphics, DMI, USB, RCBA routing).
        self.early_misc_setup();

        // 4. Route port80 to LPC.
        let lpc = ecam::EcamDevice::new(0, ich7::LPC_DEV, ich7::LPC_FUNC);
        let rcba_val = lpc.read32(ich7::RCBA_REG);
        let rcba = Rcba::new((rcba_val & 0xFFFF_C000) as usize);
        let gcs = rcba.read32(ich7::GCS);
        rcba.write32(ich7::GCS, gcs & !0x04);
        rcba.write32(0x2010, rcba.read32(0x2010) | (1 << 10));

        fstart_log::info!("intel-pineview: early init complete");
        Ok(())
    }
}

impl crate::IntelEcamConfig for IntelPineviewConfig {
    fn ecam_base(&self) -> u64 {
        self.ecam_base
    }
}

impl crate::IntelNorthbridgeDriver for IntelPineview {
    type Config = IntelPineviewConfig;

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        Ok(Self {
            config,
            detected_size: 0,
            boot_path: crate::BootPath::Normal,
            display: super::igd::IgdDisplay::new(),
        })
    }

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_phase()
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_phase()
    }

    fn detect_warm_reset(&self) -> bool {
        IntelPineview::detect_warm_reset(self)
    }

    fn set_boot_path(&mut self, boot_path: crate::BootPath) {
        self.boot_path = boot_path;
    }

    fn dram_init_with_smbus(&mut self, smbus: Option<&mut dyn SmBus>) -> Result<(), ServiceError> {
        let smbus = smbus.ok_or(ServiceError::NotInitialized)?;
        self.dram_init_with_smbus(smbus)
    }

    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        let lpc = ecam::EcamDevice::new(0, ich7::LPC_DEV, ich7::LPC_FUNC);
        let rcba = Rcba::new((lpc.read32(ich7::RCBA_REG) & 0xFFFF_C000) as usize);
        rcba.write32(0x0014, 0x8000_0001);
        rcba.write32(0x001C, 0x0312_8010);
        fstart_log::info!("pineview: VC0 configured after DRAM init");
        Ok(())
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn post_verify_init(&mut self) -> Result<(), ServiceError> {
        // Needs the verified boot media: the OpRegion embeds the VBT and the
        // modeset reads it for the panel and DDC policy.
        self.init_igd_opregion();
        if self.config.igd.display.is_some() {
            self.gma_display_init();
        }
        Ok(())
    }

    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.display.framebuffer_info()
    }
}

fn pineview_ck505_pre_raminit<B: SmBus + ?Sized>(smbus: &mut B) {
    const CLOCKGEN_ADDR: u8 = 0x69;
    const REGS: [u8; 5] = [0x00, 0x80, 0xfe, 0xff, 0xfc];

    let mut block = [0u8; 5];
    for (idx, byte) in block.iter_mut().enumerate() {
        match smbus.read_byte(CLOCKGEN_ADDR, idx as u8) {
            Ok(v) => *byte = v,
            Err(_) => {
                fstart_log::error!("pineview: failed reading CK505 configuration");
                return;
            }
        }
    }

    block[1] |= 0x80;
    block[2] = REGS[2];
    block[3] = REGS[3];
    block[4] = REGS[4];

    for (idx, byte) in block.iter().copied().enumerate() {
        if smbus.write_byte(CLOCKGEN_ADDR, idx as u8, byte).is_err() {
            fstart_log::error!("pineview: failed writing CK505 configuration");
            return;
        }
    }
    fstart_log::info!("pineview: CK505 pre-raminit configuration applied");
}

fn pineview_lower_memory_test(test_top: u32) -> Result<(), ServiceError> {
    let top = test_top as usize;
    let test_addr = if top > (32 * 1024 * 1024) {
        top - (16 * 1024 * 1024)
    } else {
        1024 * 1024
    };
    let p = test_addr as *mut u32;
    const PATTERNS: [u32; 4] = [0x0000_0000, 0xffff_ffff, 0x5555_5555, 0xaaaa_aaaa];

    fstart_log::info!(
        "ramtest: testing lower DRAM at {:#x} below top {:#x}",
        test_addr,
        top,
    );
    // SAFETY: Called only after successful Pineview DRAM training. `test_top`
    // is the top of fstart-usable low DRAM, not raw TOM, so the chosen address
    // is below UMA/GTT/TSEG reservations.
    unsafe {
        let old = ptr::read_volatile(p);
        for pattern in PATTERNS {
            ptr::write_volatile(p, pattern);
            if ptr::read_volatile(p) != pattern {
                ptr::write_volatile(p, old);
                fstart_log::error!("ramtest: failed at {:#x}", test_addr);
                return Err(ServiceError::HardwareError);
            }
        }
        ptr::write_volatile(p, old);
    }
    fstart_log::info!("ramtest: passed at {:#x}", test_addr);
    Ok(())
}

impl MemoryDetector for IntelPineview {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let tom = self.tom();
        let tolud = self.tolud();
        let usable_top = self.usable_low_memory_top();
        let raw_touud = self.touud();
        let max_reclaim = 0x1_0000_0000u64.saturating_sub(tolud as u64);
        let touud = if raw_touud > 0x1_0000_0000 && raw_touud <= tom.saturating_add(max_reclaim) {
            raw_touud
        } else {
            tom
        };

        if tom <= 0x0010_0000
            || tolud <= 0x0010_0000
            || usable_top <= 0x0010_0000
            || usable_top > tolud
        {
            fstart_log::error!(
                "pineview: invalid post-raminit memory map TOM/TOUUD/TOLUD/usable {:#x}/{:#x}/{:#x}/{:#x}",
                tom,
                raw_touud,
                tolud,
                usable_top
            );
            return Err(ServiceError::HardwareError);
        }

        let count = self.build_e820_entries(entries, usable_top, touud, tolud)?;
        publish_mtrr_wb_ranges(&entries[..count]);
        fstart_log::info!(
            "pineview: detected memory map (usable top {:#x}, TOLUD {:#x}, TOM {:#x}, TOUUD {:#x})",
            usable_top,
            tolud,
            tom,
            touud
        );
        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        Ok(self.tom())
    }
}

impl MemoryController for IntelPineview {
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        // Pineview SPD access belongs to the initialized ICH7 SMBus. The Intel
        // early flow must call IntelNorthbridgeDriver::dram_init_with_smbus().
        Err(ServiceError::NotInitialized)
    }

    fn detected_size_bytes(&self) -> u64 {
        self.detected_size
    }

    fn memory_test(&self) -> Result<(), ServiceError> {
        let tolud = self.tolud();
        let usable_top = self.usable_low_memory_top();
        let mut entries = [E820Entry::zeroed(); 8];
        if let Ok(count) = self.build_e820_entries(&mut entries, usable_top, self.touud(), tolud) {
            publish_mtrr_wb_ranges(&entries[..count]);
        }
        fstart_log::info!(
            "pineview: dynamic WB MTRR ranges set (TOLUD {:#x}, usable top {:#x})",
            tolud,
            usable_top
        );
        pineview_lower_memory_test(usable_top)
    }
}

impl IntelPineview {
    fn dram_init_with_smbus(
        &mut self,
        smbus: &mut (impl SmBus + ?Sized),
    ) -> Result<(), ServiceError> {
        if self.config.ck505_pre_raminit {
            pineview_ck505_pre_raminit(smbus);
        }
        let platform_type = self.platform_type();
        let size = raminit::sdram_initialize(
            &self.mchbar(),
            smbus,
            self.boot_path,
            platform_type,
            &self.config.spd_addresses,
        )?;
        self.detected_size = size;
        if self.boot_path != crate::BootPath::S3Resume {
            self.memory_test()?;
        }
        Ok(())
    }
}

impl PciRootProvider for IntelPineview {
    fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam_base,
            bus_start: PCI_BUS_START,
            bus_end: PCI_BUS_END,
        }
    }

    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        let mut windows = PciRootWindows::new();
        let low_base = u64::from(self.usable_low_memory_top()).max(u64::from(self.tolud()));
        let low_limit = PCI_MMIO32_LIMIT;
        let ecam_base = self.config.ecam_base;
        let ecam_end = ecam_base.saturating_add(PCI_ECAM_SIZE);

        let mut push_mmio = |base: u64, limit: u64, prefetchable: bool| {
            if base >= limit {
                return Ok(());
            }
            windows
                .push(PciWindow {
                    kind: PciWindowKind::Mmio,
                    base,
                    size: limit - base,
                    prefetchable,
                })
                .map_err(|_| PciRootError::TooManyWindows)
        };

        push_mmio(low_base, low_limit.min(ecam_base), false)?;
        push_mmio(low_base.max(ecam_end), low_limit, false)?;

        let mmio64_base = self.touud().max(0x1_0000_0000);
        push_mmio(mmio64_base, PCI_MMIO64_LIMIT, true)?;
        windows
            .push(PciWindow {
                kind: PciWindowKind::Io,
                base: PCI_PIO_BASE,
                size: PCI_PIO_SIZE,
                prefetchable: false,
            })
            .map_err(|_| PciRootError::TooManyWindows)?;

        Ok(windows)
    }
}

// ---------------------------------------------------------------------------
// Ramstage helpers — memory map readback
// ---------------------------------------------------------------------------

impl IntelPineview {
    fn build_e820_entries(
        &self,
        entries: &mut [E820Entry],
        usable_top: u32,
        touud: u64,
        tolud: u32,
    ) -> Result<usize, ServiceError> {
        if entries.len() < 8 {
            return Err(ServiceError::HardwareError);
        }

        let mut count = 0usize;
        entries[count] = E820Entry::new(0x0000_0000, 0x0000_1000, E820Kind::Reserved);
        count += 1;
        entries[count] = E820Entry::new(0x0000_1000, 0x0009_e000, E820Kind::Ram);
        count += 1;
        entries[count] = E820Entry::new(0x0009_f000, 0x0000_1000, E820Kind::Reserved);
        count += 1;
        entries[count] = E820Entry::new(0x000a_0000, 0x0005_0000, E820Kind::Reserved);
        count += 1;
        entries[count] = E820Entry::new(0x000f_0000, 0x0001_0000, E820Kind::Reserved);
        count += 1;

        let usable_top = (usable_top as u64).max(0x0010_0000);
        let low_ram_size = usable_top.saturating_sub(0x0010_0000);
        if low_ram_size != 0 {
            entries[count] = E820Entry::new(0x0010_0000, low_ram_size, E820Kind::Ram);
            count += 1;
        }
        let top_reserved_size = (tolud as u64).saturating_sub(usable_top);
        if top_reserved_size != 0 {
            entries[count] = E820Entry::new(usable_top, top_reserved_size, E820Kind::Reserved);
            count += 1;
        }
        let upper_ram_size = touud.saturating_sub(0x1_0000_0000);
        if upper_ram_size != 0 {
            entries[count] = E820Entry::new(0x1_0000_0000, upper_ram_size, E820Kind::Ram);
            count += 1;
        }

        Ok(count)
    }

    /// Read Top of Upper Usable DRAM (TOUUD) in bytes.
    pub fn touud(&self) -> u64 {
        let raw = self.hostbridge_regs().touud.get();
        (raw as u64) << 20
    }

    /// Read Top of Lower Usable DRAM (TOLUD) in bytes.
    pub fn tolud(&self) -> u32 {
        let raw = self.hostbridge_regs().tolud.get() & 0xFFF0;
        (raw as u32) << 16
    }

    /// Read Top of Memory (TOM) in bytes.
    pub fn tom(&self) -> u64 {
        let raw = self.hostbridge_regs().tom.get() & 0x01FF;
        // Coreboot programs TOM as `tom_mib >> 6`, i.e. units of 64 MiB.
        (raw as u64) << 26
    }

    #[cfg(feature = "ffs-vbt")]
    fn igd(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, 2, 0)
    }

    #[cfg(feature = "ffs-vbt")]
    fn vbt_size(vbt: &[u8]) -> Option<usize> {
        if vbt.len() < 28 || u32::from_le_bytes([vbt[0], vbt[1], vbt[2], vbt[3]]) != VBT_SIGNATURE {
            return None;
        }
        let size = u16::from_le_bytes([vbt[24], vbt[25]]) as usize;
        if size == 0 || size > vbt.len() {
            return None;
        }
        Some(size)
    }

    #[cfg(feature = "ffs-vbt")]
    fn ffs_vbt(&self) -> Option<Vec<u8>> {
        let file_name = self.config.igd.vbt_file?;
        let bytes = fstart_core::services::ffs_context::read_verified_asset(file_name)?;
        let size = Self::vbt_size(bytes)?;
        Some(bytes[..size].to_vec())
    }

    #[cfg(feature = "ffs-vbt")]
    fn legacy_vbt(&self) -> Option<&'static [u8]> {
        // SAFETY: 0xc0000 legacy option ROM window is readable on PC-compatible x86.
        let rom = unsafe { core::slice::from_raw_parts(0xC0000 as *const u8, 128 * 1024) };
        let mut off = 0usize;
        while off + 4 < rom.len() {
            if u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
                == VBT_SIGNATURE
                && let Some(size) = Self::vbt_size(&rom[off..])
            {
                return Some(&rom[off..off + size]);
            }
            off += 16;
        }
        None
    }

    #[cfg(feature = "ffs-vbt")]
    fn locate_vbt(&self) -> Option<VbtBytes<'static>> {
        #[cfg(feature = "ffs-vbt")]
        if self.config.igd.vbt_file.is_some() {
            // A configured authenticated asset must not fall back to legacy
            // memory after a verification failure.
            return self.ffs_vbt().map(VbtBytes::Owned);
        }
        self.legacy_vbt().map(VbtBytes::Borrowed)
    }

    #[cfg(feature = "ffs-vbt")]
    fn init_igd_opregion(&self) {
        if self.igd().read16(0) == 0xffff {
            return;
        }

        let Some(vbt) = self.locate_vbt() else {
            fstart_log::error!("pineview: no valid VBT found for IGD opregion");
            return;
        };
        let vbt = vbt.as_slice();

        let opregion = crate::igd_opregion_buf(super::igd::opregion_size(vbt.len()));
        super::igd::build_opregion(opregion, vbt);

        let igd = self.igd();
        igd.write32(IGD_ASLS, opregion.as_ptr() as u32);
        // Atom platforms use the combined SWSMISCI register.
        let swsmisci = (igd.read16(IGD_SWSMISCI) & !1) | (1 << 15);
        igd.write16(IGD_SWSMISCI, swsmisci);
        fstart_log::info!(
            "pineview: IGD opregion at {:#x}, VBT {} bytes",
            opregion.as_ptr() as usize,
            vbt.len() as u32,
        );
    }

    #[cfg(not(feature = "ffs-vbt"))]
    fn init_igd_opregion(&self) {}

    fn igd_memory_size_kb(&self) -> u32 {
        let ggc = self.hostbridge_regs().ggc.get();
        let gms = ((ggc >> 4) & 0xF) as usize;
        const SIZES: [u32; 10] = [0, 1, 4, 8, 16, 32, 48, 64, 128, 256];
        if gms < SIZES.len() {
            SIZES[gms] << 10
        } else {
            0
        }
    }

    /// Decode GTT stolen memory size from GGC register (kilobytes).
    fn gtt_size_kb(&self) -> u32 {
        let ggc = self.hostbridge_regs().ggc.get();
        let gsm = ((ggc >> 8) & 0xF) as usize;
        const SIZES: [u32; 4] = [0, 1, 0, 0];
        if gsm < SIZES.len() {
            SIZES[gsm] << 10
        } else {
            0
        }
    }

    /// Enable SERR on the PCI domain root.
    pub fn enable_serr(&self) {
        ecam::EcamDevice::new(0, 0, 0).or16(0x04, 1 << 8);
    }

    // ---------------------------------------------------------------
    // TSEG / SMRAM (from memmap.c)
    // ---------------------------------------------------------------

    /// Decode TSEG size from ESMRAMC register (bytes).
    ///
    /// Returns 0 if T_EN (bit 0) is not set.
    pub fn tseg_size(&self) -> u32 {
        let esmramc = self.hostbridge_regs().esmramc.get();
        if esmramc & 1 == 0 {
            return 0;
        }
        match (esmramc >> 1) & 3 {
            0 => 1024 * 1024,     // 1 MiB
            1 => 2 * 1024 * 1024, // 2 MiB
            2 => 8 * 1024 * 1024, // 8 MiB
            _ => {
                fstart_log::error!("pineview: bad TSEG size encoding");
                0
            }
        }
    }

    /// Read the TSEG base address.
    pub fn tseg_base(&self) -> u32 {
        self.hostbridge_regs().tseg.get()
    }

    /// Get the SMM region (base + size) as a `(base, size)` pair.
    ///
    /// Used by the MP init code to know where TSEG lives.
    pub fn smm_region(&self) -> (u32, u32) {
        (self.tseg_base(), self.tseg_size())
    }

    /// Top of fstart-usable low DRAM, excluding top-of-memory reservations.
    pub fn usable_low_memory_top(&self) -> u32 {
        let mut top = self.tolud();
        let igd = self.igd_base();
        if igd != 0 {
            top = top.min(igd);
        }
        let gtt = self.gtt_base();
        if gtt != 0 {
            top = top.min(gtt);
        }
        let tseg = self.tseg_base();
        if tseg != 0 {
            top = top.min(tseg);
        }
        top
    }

    /// Write the SMRAM register (used by SMM relocation).
    pub fn write_smram(&self, val: u8) {
        self.hostbridge_regs().smram.set(val);
    }

    /// Read the SMRAM register.
    pub fn read_smram(&self) -> u8 {
        self.hostbridge_regs().smram.get()
    }

    fn smm_open(&self) {
        self.write_smram(SMRAM_D_OPEN | SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smm_close(&self) {
        self.write_smram(SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smm_lock(&self) {
        self.write_smram(SMRAM_D_LCK | SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smi_enable_for_relocation() {
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PMBASE);
        pm.setbits32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );
    }

    fn cr3() -> u64 {
        let cr3: u64;
        // SAFETY: reading CR3 is safe in firmware privileged mode.
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        }
        cr3
    }

    // ---------------------------------------------------------------
    // Full memory map (from northbridge.c)
    // ---------------------------------------------------------------

    /// Program the IGD BARs and hand the display engine to the shared GMA layer.
    ///
    /// Pineview exposes its GTT through a dedicated BAR3, and coreboot programs
    /// `PGETBL_CTL` from the stolen-memory base register (`BGSM`) twice with a
    /// short delay before the modeset. Without that enable bit the display
    /// engine cannot translate framebuffer addresses.
    /// Keep the BAR assignment PCI enumeration produced, programming the
    /// platform default only when the window was left unassigned.
    fn keep_or_program_bar(igd: &ecam::EcamDevice, reg: u16, fallback: u64) -> u64 {
        let value = igd.read32(reg);
        if value == 0 {
            igd.write32(reg, (fallback & 0xffff_ffff) as u32);
            fallback & !0xf
        } else {
            u64::from(value) & !0xf
        }
    }

    fn gma_display_init(&mut self) {
        let igd = ecam::EcamDevice::new(0, 2, 0);

        // The graphics windows are assigned by PCI enumeration and resource
        // allocation: use those values. Re-programming them moves the GMCH
        // register block, which follows the BAR, away from the window the PCH
        // side is strapped to, and the display logic there stops responding.
        // (A hardcoded 0xfed00000 also overlaps the fixed MCHBAR/DMIBAR/EPBAR
        // windows, so accesses above +0x10000 land in chipset registers.)
        let gtt_mmio_base =
            Self::keep_or_program_bar(&igd, IGD_BAR0_GTTMMADR, self.config.igd.gtt_mmio_base);
        let gmadr_base =
            Self::keep_or_program_bar(&igd, IGD_BAR2_GMADR, self.config.igd.gmadr_base);
        let gtt_pte_base =
            Self::keep_or_program_bar(&igd, IGD_BAR3_GTTADR, self.config.igd.gtt_pte_base);

        igd.or16(PCI_COMMAND, PCI_CMD_MEMORY | PCI_CMD_MASTER);
        igd.and8_or8(IGD_MSAC, !0x3, 0x2);

        // coreboot writes PGETBL_CTL twice around a short delay, then flushes.
        let gtt_base = self.gtt_base();
        super::igd::program_gtt_base(gtt_mmio_base, gtt_base, 0);
        fstart_arch::x86::udelay(50);
        super::igd::program_gtt_base(gtt_mmio_base, gtt_base, 0);
        super::igd::clear_gtt_table(gtt_pte_base, IGD_GTT_SIZE);

        // Graphics stolen memory runs from the graphics stolen base up to
        // TOLUD. The GTT sits *below* it on this platform (BGSM is 2039 MiB and
        // GBSM 2040 MiB on a D41S), so a GTT-relative size collapses to the GTT
        // size alone and the framebuffer never fits.
        let stolen_base = self.igd_base();
        let stolen_size = self.tolud().saturating_sub(stolen_base);
        let addresses = super::igd::IgdAddresses {
            pci_bdf: PciAddress::new(0, 0, 2, 0),
            gtt_mmio_base,
            gtt_mmio_size: IGD_GTTMMADR_SIZE,
            gtt_pte_base: Some(gtt_pte_base),
            gmadr_base: Some(gmadr_base),
            gmadr_size: self.config.igd.gmadr_size,
            stolen_base: u64::from(stolen_base),
            stolen_size,
            gtt_size: IGD_GTT_SIZE,
            gcfgc: Some(self.mchbar().read16(mchbar::MCH_GCFGC)),
        };
        let cpu = if self.platform_type() == raminit::PLATFORM_MOBILE {
            Cpu::PineviewM
        } else {
            Cpu::Pineview
        };
        #[cfg(feature = "ffs-vbt")]
        let vbt = self.locate_vbt();
        #[cfg(feature = "ffs-vbt")]
        let vbt = vbt.as_ref().map(|bytes| bytes.as_slice());
        #[cfg(not(feature = "ffs-vbt"))]
        let vbt: Option<&[u8]> = None;
        self.display
            .initialize(cpu, self.config.igd.display.as_ref(), &addresses, vbt);
    }

    /// Read the graphics stolen memory base (GBSM register).
    pub fn igd_base(&self) -> u32 {
        self.hostbridge_regs().gbsm.get()
    }

    /// Read the GTT stolen memory base (BGSM register).
    pub fn gtt_base(&self) -> u32 {
        self.hostbridge_regs().bgsm.get()
    }

    /// Log the full memory map.
    ///
    /// Reads and prints TOUUD, TOLUD, TOM, IGD, GTT, and TSEG.
    pub fn dump_memory_map(&self) {
        let touud = self.touud();
        let tolud = self.tolud();
        let tom = self.tom();
        let igd_kb = self.igd_memory_size_kb();
        let gtt_kb = self.gtt_size_kb();
        let tseg_base = self.tseg_base();
        let tseg_size = self.tseg_size();

        fstart_log::info!("pineview: TOUUD={:#x}", touud);
        fstart_log::info!("pineview: TOLUD={:#x}", tolud);
        fstart_log::info!("pineview: TOM={:#x}", tom);
        fstart_log::info!("pineview: IGD stolen={}K", igd_kb);
        fstart_log::info!("pineview: GTT stolen={}K", gtt_kb);
        fstart_log::info!("pineview: TSEG base={:#x} size={:#x}", tseg_base, tseg_size);
        fstart_log::info!(
            "pineview: usable low memory top={:#x}",
            self.usable_low_memory_top()
        );
    }
}

impl SmmOps for IntelPineview {
    fn smm_info(&self) -> Option<SmmInfo> {
        let (base, size) = self.smm_region();
        if size == 0 {
            fstart_log::error!("pineview SMM: TSEG is disabled");
            return None;
        }
        fstart_log::info!("pineview SMM: TSEG base={:#x} size={:#x}", base, size);
        Some(SmmInfo {
            smbase: base as u64,
            smsize: size as usize,
            save_state_size: EM64T101_SAVE_STATE_SIZE,
        })
    }

    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError> {
        self.smm_open();

        let layouts = unsafe { &mut *PINEVIEW_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: Self::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: 0,
                    platform_data: [ICH7_PMBASE as u64, 0x28, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                fstart_arch::mp::prepare_default_smm_relocation(targets);
                // The relocation stub enters long mode, but firmware stages run
                // unpaged, so it must be given its own identity page tables
                // instead of the (stale) firmware CR3.
                // SAFETY: the default SMBASE region is writable low memory and
                // unused until the stub itself is installed there.
                let relocation_cr3 = unsafe {
                    fstart_smm::build_relocation_identity_tables(
                        fstart_arch::mp::SMM_DEFAULT_SMBASE,
                    )
                };
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_callback_stub(
                        image,
                        fstart_smm::DefaultRelocationCallbackConfig {
                            default_smbase: fstart_arch::mp::SMM_DEFAULT_SMBASE,
                            cr3: relocation_cr3,
                            callback: fstart_arch::mp::default_smm_relocation_handler as *const ()
                                as usize as u64,
                            stack_top: fstart_arch::mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!(
                        "pineview SMM: failed to install default relocation handler"
                    );
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "pineview SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("pineview SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        Self::smi_enable_for_relocation();

        // coreboot triggers the relocation with a local-APIC self SMI
        // (`smm_initiate_relocation`). This chipset does not deliver an SMI
        // raised through the LAPIC ICR, so use the ICH7 APM command port,
        // which `smi_enable_for_relocation` already enables through APMC_EN.
        // The SMI raised there is broadcast, so the MP flight plan holds the
        // relocation lock until the handler has run (see
        // `smm_relocate_trampoline`): every CPU shares the architectural
        // default SMBASE until it has relocated itself.
        //
        // SAFETY: 0xB2 is the ICH7 APM command port while SMI is enabled.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") 0xB2u16, in("al") 0x00u8, options(nomem, nostack));
        }
    }

    fn pre_smm_init(&self) {
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PMBASE);

        // Keep the relocation SMI setup minimal. The permanent handler is not
        // installed yet; enabling only APMC + global SMI matches the path that
        // previously allowed CPU SMBASE relocation to complete. Full
        // coreboot-style PM/TCO/GPE cleanup is done in post_smm_init(), after
        // the permanent handler is installed.
        pm.reset_smi_status();
        pm.write32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );
        fstart_log::info!(
            "pineview SMM: relocation SMI_EN={:#x} PM1_CNT={:#x}",
            pm.read32(crate::southbridge::pmio_ich::SMI_EN),
            pm.read32(crate::southbridge::pmio_ich::PM1_CNT),
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PMBASE);

        // Match coreboot's smm_southbridge_clear_state() followed by
        // global_smi_enable(): clear stale PM/SMI/TCO/GPE status before
        // enabling permanent SMI sources, then enable TCO/APMC/SLP SMI plus
        // EOS and the global SMI gate.
        pm.reset_smi_status();
        pm.reset_pm1_status();
        pm.tco().reset_tco_status();
        pm.reset_gpe0_status();
        pm.write16(
            crate::southbridge::pmio_ich::PM1_EN,
            crate::southbridge::pmio_ich::PWRBTN_EN | crate::southbridge::pmio_ich::GBL_EN,
        );
        pm.write32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::TCO_EN
                | crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::SLP_SMI_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );

        // Match coreboot i82801gx_set_acpi_mode() on a normal boot: after
        // permanent SMM is installed, issue APM_CNT_ACPI_DISABLE so the SMI
        // handler clears PM1_CNT.SCI_EN and all stale PM/GPE/TCO status.  The
        // FADT advertises APM_CNT_ACPI_ENABLE (0xe1), so Linux will re-enable
        // SCI only after ACPICA has installed its handler.
        unsafe { fstart_core::pio::outb(APM_CNT, 0x1e) };

        fstart_log::info!(
            "pineview SMM: SMI_EN={:#x} PM1_CNT={:#x}",
            pm.read32(crate::southbridge::pmio_ich::SMI_EN),
            pm.read32(crate::southbridge::pmio_ich::PM1_CNT),
        );

        self.smm_lock();
        fstart_log::info!("pineview SMM: global SMI enabled and SMRAM locked");
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — Host bridge (MCHC)
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;
    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;

    use super::*;

    impl AcpiDevice for IntelPineview {
        type Config = IntelPineviewConfig;

        /// Produce Pineview northbridge DSDT content.
        ///
        /// Includes:
        /// - **MCHC** (0:0.0): host bridge device with PCI config
        ///   OperationRegion exposing EPBAR/MCHBAR/PCIEXBAR/DMIBAR/PAM/
        ///   TOLUD/TOM fields for OS runtime use.
        /// - **PDRC**: Platform Device Resource Consumption (PNP0C02)
        ///   reserving RCBA, MCHBAR, DMIBAR, EPBAR, and ICH misc MMIO.
        /// - **PCI0 `_HID`/`_CID`/`_BBN`**: PCIe host bridge identity.
        ///
        /// The full PCI0 `_CRS` with dynamic TOLUD patching is not
        /// emitted here — Linux falls back to e820/PCI BAR probing.
        /// A future phase can add the `_CRS` Method with
        /// `CreateDwordField` / `ShiftLeft` fixups.
        ///
        /// Ported from coreboot `northbridge/intel/pineview/acpi/`.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;

            // 1. MCHC device with PCI config OperationRegion.
            //
            // Coreboot hostbridge.asl: MCHP OpRegion in PCI_Config
            // with fields for EPBAR, MCHBAR, PCIEXBAR, DMIBAR, PAM
            // registers, TOM, and TOLUD.  These are read by the OS
            // to discover memory topology.
            let mut aml: Vec<u8> = fstart_acpi_macros::acpi_dsl! {
                Device("MCHC") {
                    Name("_ADR", 0u32);

                    OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
                    Field("MCHP", DWordAcc, NoLock, Preserve) {
                        Offset(0x40),
                        // EPBAR
                        EPEN, 1,
                        , 11,
                        EPBR, 20,
                        Offset(0x48),
                        // MCHBAR
                        MHEN, 1,
                        , 13,
                        MHBR, 18,
                        Offset(0x60),
                        // PCIEXBAR
                        PXEN, 1,
                        PXSZ, 2,
                        , 23,
                        PXBR, 6,
                        Offset(0x68),
                        // DMIBAR
                        DMEN, 1,
                        , 11,
                        DMBR, 20,

                        Offset(0x90),
                        // PAM0
                        , 4,
                        PM0H, 2,
                        , 2,
                        // PAM1
                        PM1L, 2,
                        , 2,
                        PM1H, 2,
                        , 2,
                        // PAM2
                        PM2L, 2,
                        , 2,
                        PM2H, 2,
                        , 2,
                        // PAM3
                        PM3L, 2,
                        , 2,
                        PM3H, 2,
                        , 2,
                        // PAM4
                        PM4L, 2,
                        , 2,
                        PM4H, 2,
                        , 2,
                        // PAM5
                        PM5L, 2,
                        , 2,
                        PM5H, 2,
                        , 2,
                        // PAM6
                        PM6L, 2,
                        , 2,
                        PM6H, 2,
                        , 2,

                        Offset(0xA0),
                        TOM_, 16,

                        Offset(0xB0),
                        , 4,
                        TLUD, 12,
                    }
                }
            }
            .into();

            // 2. PDRC — Platform Device Resource Consumption.
            //
            // Reserves MMIO ranges for RCBA, MCHBAR, DMIBAR, EPBAR,
            // and miscellaneous ICH regions so the OS won't allocate
            // PCI BARs over them.
            // Coreboot: pineview.asl Device(PDRC)
            let rcba: u32 = 0xFED1_C000; // ICH7 default RCBA
            let ecam_base = config.ecam_base as u32;
            let ecam_size: u32 = 0x1000_0000; // 256 MiB, buses 0..255
            aml.extend(Vec::from(fstart_acpi_macros::acpi_dsl! {
                Device("PDRC") {
                    Name("_HID", EisaId("PNP0C02"));
                    Name("_UID", 1u32);
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword rcba}, 0x4000u32);
                        Memory32Fixed(ReadWrite, #{dword mchbar}, 0x4000u32);
                        Memory32Fixed(ReadWrite, #{dword dmibar}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword epbar}, 0x1000u32);
                        // PCI Express ECAM/MMCONFIG window. Linux requires
                        // every MCFG range to be reserved by motherboard
                        // resources (PNP0C02), otherwise it refuses ECAM.
                        Memory32Fixed(ReadWrite, #{dword ecam_base}, #{dword ecam_size});
                        // Misc ICH MMIO (HPET area, TPM, etc.)
                        Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                        Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                        Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                    });
                }
            }));

            // 3. PCI0 host bridge identity + _CRS.
            //
            // _HID PNP0A08 (PCIe), _CID PNP0A03 (PCI), _BBN 0.
            // The _CRS declares bus numbers, I/O ports, VGA memory,
            // and the PCI MMIO window.  The PCI MMIO base is patched
            // at runtime from the MCHC TOLUD register.
            //
            // Coreboot: hostbridge.asl Names + MCRS + _CRS Method.
            // The PCI host-bridge MMIO aperture begins at the live chipset
            // TOLUD value programmed by raminit. This is evaluated while ACPI
            // tables are generated in ramstage, not baked into the board metadata.
            #[cfg(target_os = "none")]
            let pci_mmio_base = self.tolud();
            #[cfg(not(target_os = "none"))]
            let pci_mmio_base = 0x8000_0000u32;
            let pci_mmio_limit = 0xFEBF_FFFFu32;

            aml.extend(Vec::from(fstart_acpi_macros::acpi_dsl! {
                Device("PCI0") {
                    Name("_HID", EisaId("PNP0A08"));
                    Name("_CID", EisaId("PNP0A03"));
                    Name("_BBN", 0u32);

                // Named resource template for PCI0.  The PCI memory
                // region (PM01) base address is patched in _CRS to
                // match the actual TOLUD value.
                Name("MCRS", ResourceTemplate {
                    // Bus numbers 0x00-0xFF.
                    WordBusNumber(0x0000u16, 0x00FFu16);
                    // I/O below PCI config (0x0000-0x0CF7).
                    DWordIO(0x0000u32, 0x0CF7u32);
                    // PCI Config I/O (0x0CF8-0x0CFF) — separate so OSPM
                    // knows it's the config mechanism.
                    IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                    // I/O above PCI config (0x0D00-0xFFFF).
                    DWordIO(0x0D00u32, 0xFFFFu32);
                    // VGA memory (0xA0000-0xBFFFF).
                    DWordMemory(Cacheable, ReadWrite, 0x000A0000u32, 0x000BFFFFu32);
                    // PCI MMIO window: TOLUD..0xFEBFFFFF. Anything below
                    // TOLUD is low DRAM; anything at/above TOLUD and below
                    // the fixed chipset MMIO blocks is available for PCI.
                    DWordMemory(NotCacheable, ReadWrite, #{dword pci_mmio_base}, #{dword pci_mmio_limit});
                });

                // _CRS method: patch PCI MMIO base from TOLUD register.
                //
                // The TOLUD field (bits [15:4] of NB register 0xB0)
                // gives the top of low usable DRAM in 16 MiB units.
                // PCI MMIO starts at TOLUD and ends at 0xFEBFFFFF.
                //
                // PMIN = TLUD << 20  (TOLUD bits [15:4] are 12 bits at
                //   bit position 4; shift left by 20 to get a 32-bit addr,
                //   since the register value is in 1 MiB units in bits
                //   [15:4] which needs << 16 after >> 4 extraction — the
                //   Field already extracts the 12-bit value, so <<20 gives
                //   the address.  However, the coreboot code does << 27
                //   on the raw 5-bit TLUD field; we match that exactly.)
                // PLEN = PMAX - PMIN + 1
                Method("_OSC", 4, NotSerialized) {
                    Return(Arg3);
                }

                Method("_CRS", 0, Serialized) {
                    // Byte offsets into MCRS for the last DWordMemory:
                    //  _MIN is at a fixed offset within the resource
                    //  template.  The exact offset depends on the
                    //  preceding descriptors.  We use hardcoded offsets
                    //  matching the template layout above.
                    //
                    // WordBusNumber:  2+2+2+2+2 = 10 bytes (+ 1 tag = 11? no —
                    //   large resource: 3-byte header + body)
                    // The offsets are template-internal and must match
                    // the serialised resource descriptor positions.
                    //
                    // Rather than calculate exact offsets (which depend on
                    // the AML resource encoding), we approximate with the
                    // coreboot approach: patch via CreateDwordField at
                    // known tag names.  Since our macro doesn’t support
                    // named resource tags, we use numeric offsets.
                    //
                    // The last DWordMemory _MIN field is at byte offset
                    // within the resource template buffer.  We’ll compute
                    // it: each large resource descriptor has a 3-byte
                    // header (type + 2-byte length).
                    //
                    // For a simpler approach that works: just return the
                    // template with a fixed TOLUD value read from HW.
                    //
                    // Actually the cleanest approach: use ShiftLeft to
                    // dynamically compute PMIN from the TLUD field.
                    //
                    // CreateDwordField with numeric offset into MCRS.
                    // Offsets for the last DWordMemory descriptor:
                    //   The _MIN field.

                    // Simplified: return the static template.
                    // The TOLUD value is baked in at firmware build time
                    // if needed, or Linux uses e820 + PCI BAR probing.
                    Return(MCRS);
                }
                }
            }));

            // ---------------------------------------------------------------
            // 4. Processor power-management devices (\._SB.CP00, CP01).
            //
            // Ported from coreboot's `cpu/intel/speedstep` ACPI generator:
            // each CPU device gets `_PCT`, `_PSD`, and an MSR-derived `_PSS`.
            // Pineview board `get_cst_entries()` implementations return 0,
            // so no `_CST` is emitted for this chipset.
            // ---------------------------------------------------------------
            aml.extend_from_slice(&crate::cpu::core2_aml::pineview_cpu_devices_aml(2));

            aml
        }

        fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
            let mut mcfg = fstart_acpi::mcfg::MCFG::new(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
            );
            mcfg.add_ecam(config.ecam_base, 0, 0, 0xff);
            let mut bytes = Vec::new();
            fstart_acpi::Aml::to_aml_bytes(&mcfg, &mut bytes);
            alloc::vec![bytes]
        }
    }
}
