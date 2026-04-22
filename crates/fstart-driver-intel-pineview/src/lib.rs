//! Intel Atom D4xx/D5xx (Pineview) northbridge driver.
//!
//! Covers the integrated memory controller hub on the Atom D410/D510/D525
//! family. Responsibilities:
//!
//! - **Early init ([`PciHost::early_init`])**: enable ECAM (PCIEXBAR) via
//!   the single legacy CF8/CFC write, then use ECAM MMIO for everything:
//!   MCHBAR/DMIBAR/EPBAR setup, PAM unlock, graphics clocks, and
//!   miscellaneous chipset init.
//! - **DRAM training ([`MemoryController::init`])**: full DDR2 raminit.
//!   **Currently a stub** — a future phase will port the ~2600-line
//!   coreboot `raminit.c`.
//!
//! Register definitions live in `fstart-pineview-regs`.

#![no_std]

pub mod raminit;

use fstart_pineview_regs::{ecam, hostbridge, ich7, mchbar, DmiBar, MchBar, Rcba};
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_controller::MemoryController;
use fstart_services::{PciHost, ServiceError};
use serde::{Deserialize, Serialize};

/// Intel integrated graphics configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IgdConfig {
    /// Enable the VGA CRT output.
    #[serde(default)]
    pub use_crt: bool,
    /// Enable the LVDS panel output.
    #[serde(default)]
    pub use_lvds: bool,
    /// Enable PLL spread spectrum.
    #[serde(default)]
    pub spread_spectrum: bool,
}

/// Pineview northbridge configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IntelPineviewConfig {
    /// MCHBAR base address.
    pub mchbar: u64,
    /// DMIBAR base address.
    pub dmibar: u64,
    /// EPBAR base address.
    pub epbar: u64,
    /// ECAM (PCIEXBAR) base address. Default: `0xE000_0000`.
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// Optional integrated graphics configuration.
    #[serde(default)]
    pub igd: Option<IgdConfig>,
}

fn default_ecam_base() -> u64 {
    hostbridge::DEFAULT_ECAM_BASE as u64
}

/// Pineview NB driver.
pub struct IntelPineview {
    config: IntelPineviewConfig,
    /// Detected DRAM size (bytes), populated by `init()`.
    detected_size: u64,
}

// SAFETY: Driver holds no unsynchronized shared state; MMIO and PCI
// config writes are CPU-exclusive in firmware.
unsafe impl Send for IntelPineview {}
unsafe impl Sync for IntelPineview {}

impl IntelPineview {
    /// ECAM accessor for this platform.

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
            fstart_pio::pci_cfg_write32(0, 0, 0, hostbridge::PCIEXBAR as u8, pciexbar_val);
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
        // EPBAR, MCHBAR, DMIBAR — 32-bit writes with enable bit 0.
        ecam::write32(0, 0, 0, hostbridge::EPBAR, (self.config.epbar as u32) | 1);
        ecam::write32(0, 0, 0, hostbridge::MCHBAR, (self.config.mchbar as u32) | 1);
        ecam::write32(0, 0, 0, hostbridge::DMIBAR, (self.config.dmibar as u32) | 1);
        ecam::write32(
            0,
            0,
            0,
            hostbridge::PMIOBAR,
            hostbridge::DEFAULT_PMIOBAR | 1,
        );

        // DEVEN — enable D0F0, D2F0, D2F1.
        ecam::write8(0, 0, 0, hostbridge::DEVEN, hostbridge::BOARD_DEVEN);

        // PAM0..PAM6: unlock BIOS shadow region C0000–FFFFF for RAM r/w.
        ecam::write8(0, 0, 0, hostbridge::PAM0, 0x30);
        ecam::write8(0, 0, 0, hostbridge::PAM1, 0x33);
        ecam::write8(0, 0, 0, hostbridge::PAM2, 0x33);
        ecam::write8(0, 0, 0, hostbridge::PAM3, 0x33);
        ecam::write8(0, 0, 0, hostbridge::PAM4, 0x33);
        ecam::write8(0, 0, 0, hostbridge::PAM5, 0x33);
        ecam::write8(0, 0, 0, hostbridge::PAM6, 0x33);

        fstart_log::info!("pineview: northbridge BARs and PAM configured");
    }

    /// Graphics clock and output setup.
    ///
    /// Ported from coreboot `early_graphics_setup()`.
    fn early_graphics_setup(&self) {
        let mch = self.mchbar();

        // GGC: 1 MiB GTT (GGMS=1), 8 MiB stolen (GMS=3).
        ecam::write16(0, 0, 0, hostbridge::GGC, (1 << 8) | (3 << 4));

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
        let cc_val = ecam::read16(0, 2, 0, 0xCC) & !0x1FF;
        ecam::write16(0, 2, 0, 0xCC, cc_val | igd_cc);

        ecam::and8(0, 2, 0, 0x62, !0x3);
        ecam::or8(0, 2, 0, 0x62, 2);

        // VGA CRT / LVDS output control.
        if let Some(ref igd) = self.config.igd {
            if igd.use_crt {
                mch.setbits32(mchbar::DACGIOCTRL1, 1 << 15);
            } else {
                mch.clrbits32(mchbar::DACGIOCTRL1, 1 << 15);
            }
            if igd.use_lvds {
                let reg = mch.read32(mchbar::LVDSICR2);
                mch.write32(mchbar::LVDSICR2, (reg & !0xF100_0000) | 0x9000_0000);
                mch.setbits32(mchbar::IOCKTRR1, 1 << 9);
            } else {
                mch.setbits32(mchbar::DACGIOCTRL1, 3 << 25);
            }
        }

        mch.write32(mchbar::CICTRL, 0xC6DB_8B5F);
        mch.write16(mchbar::CISDCTRL, 0x024F);

        mch.clrbits32(mchbar::DACGIOCTRL1, 0xFF);
        mch.setbits32(mchbar::DACGIOCTRL1, 1 << 5);

        // Legacy backlight control.
        ecam::write8(0, 2, 0, 0xF4, 0x4C);

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
        ecam::write32(0, 0x1e, 0, 0x18, 0x0002_0200);
        ecam::write32(0, 0x1e, 0, 0x18, 0x0000_0000);

        self.early_graphics_setup();

        // HIT4 sequence.
        mch.read32(mchbar::HIT4);
        mch.write32(mchbar::HIT4, 0);
        mch.read32(mchbar::HIT4);
        mch.write32(mchbar::HIT4, 1 << 3);

        // LPC device (1F:0) revision ID reset sequence.
        ecam::write8(0, ich7::LPC_DEV, ich7::LPC_FUNC, 0x08, 0x1D);
        ecam::write8(0, ich7::LPC_DEV, ich7::LPC_FUNC, 0x08, 0x00);

        // RCBA routing registers. Read RCBA from ICH7 LPC config.
        let rcba_val = ecam::read32(0, ich7::LPC_DEV, ich7::LPC_FUNC, ich7::RCBA_REG);
        let rcba = Rcba::new((rcba_val & 0xFFFF_C000) as usize);

        rcba.write32(0x3410, 0x0002_0465);

        // USB transient disconnect (1D:0..3 reg 0xCA).
        for func in 0..4u8 {
            ecam::or32(0, 0x1d, func, 0xCA, 0x1);
        }

        // RCBA routing table setup.
        rcba.write32(0x3100, 0x0004_2210);
        rcba.write32(0x3108, 0x1000_4321);
        rcba.write32(0x310C, 0x0021_4321);
        rcba.write32(0x3110, 1);
        rcba.write32(0x3140, 0x0146_0132);
        rcba.write32(0x3142, 0x0237_0146);
        rcba.write32(0x3144, 0x3201_0237);
        rcba.write32(0x3146, 0x0146_3201);
        rcba.write32(0x3148, 0x0000_0146);

        fstart_log::info!("pineview: early misc setup complete");
    }
}

impl Device for IntelPineview {
    const NAME: &'static str = "intel-pineview";
    const COMPATIBLE: &'static [&'static str] = &["intel,pineview-mch", "intel,atom-d4xx-mch"];
    type Config = IntelPineviewConfig;

    fn new(config: &IntelPineviewConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            config: *config,
            detected_size: 0,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        fstart_log::info!("intel-pineview: mchbar={:#x}", self.config.mchbar);

        // DRAM training runs when referenced by a `DramInit` capability.
        // The actual training is called from the generated board adapter's
        // `dram_init()` trampoline, which passes in the SMBus handle and
        // SPD addresses.  The `init()` here is for non-DRAM device setup.
        //
        // If no DramInit capability ran (e.g., ramstage re-init), assume
        // DRAM is already trained and read size from DRB registers.
        if self.detected_size == 0 {
            let mch = self.mchbar();
            let drb3 = mch.read16(fstart_pineview_regs::mchbar::C0DRB0 + 6);
            self.detected_size = (drb3 as u64) * 32 * 1024 * 1024;
            if self.detected_size == 0 {
                fstart_log::warn!("intel-pineview: DRAM not yet trained");
            }
        }
        Ok(())
    }
}

impl PciHost for IntelPineview {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        // 1. Enable ECAM (single legacy CF8/CFC write).
        self.enable_ecam();

        // 2. Program BARs + PAM via ECAM.
        self.setup_bars();

        // 3. Miscellaneous chipset init (graphics, DMI, USB, RCBA routing).
        self.early_misc_setup();

        // 4. Route port80 to LPC.
        let rcba_val = ecam::read32(0, ich7::LPC_DEV, ich7::LPC_FUNC, ich7::RCBA_REG);
        let rcba = Rcba::new((rcba_val & 0xFFFF_C000) as usize);
        let gcs = rcba.read32(ich7::GCS);
        rcba.write32(ich7::GCS, gcs & !0x04);
        rcba.write32(0x2010, rcba.read32(0x2010) | (1 << 10));

        // 5. Virtual Channel 0 setup (from romstage rcba_config()).
        rcba.write32(0x0014, 0x8000_0001);
        rcba.write32(0x001C, 0x0312_8010);

        fstart_log::info!("intel-pineview: early init complete");
        Ok(())
    }
}

impl MemoryController for IntelPineview {
    fn detected_size_bytes(&self) -> u64 {
        self.detected_size
    }
}
