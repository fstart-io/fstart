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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// ACPI device name (e.g., "MCHC"). If `None`, no ACPI node.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
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
            config: config.clone(),
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

// ---------------------------------------------------------------------------
// Ramstage helpers — memory map readback
// ---------------------------------------------------------------------------

impl IntelPineview {
    /// Read Top of Upper Usable DRAM (TOUUD) in bytes.
    pub fn touud(&self) -> u64 {
        let raw = ecam::read16(0, 0, 0, hostbridge::TOUUD);
        (raw as u64) << 20
    }

    /// Read Top of Lower Usable DRAM (TOLUD) in bytes.
    pub fn tolud(&self) -> u32 {
        let raw = ecam::read16(0, 0, 0, hostbridge::TOLUD) & 0xFFF0;
        (raw as u32) << 16
    }

    /// Read Top of Memory (TOM) in bytes.
    pub fn tom(&self) -> u64 {
        let raw = ecam::read16(0, 0, 0, hostbridge::TOM) & 0x01FF;
        (raw as u64) << 27
    }

    /// Decode IGD memory size from GGC register (kilobytes).
    fn igd_memory_size_kb(&self) -> u32 {
        let ggc = ecam::read16(0, 0, 0, hostbridge::GGC);
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
        let ggc = ecam::read16(0, 0, 0, hostbridge::GGC);
        let gsm = ((ggc >> 8) & 0xF) as usize;
        const SIZES: [u32; 4] = [0, 1, 0, 0];
        if gsm < SIZES.len() {
            (SIZES[gsm] as u32) << 10
        } else {
            0
        }
    }

    /// Enable SERR on the PCI domain root.
    pub fn enable_serr(&self) {
        ecam::or16(0, 0, 0, 0x04, 1 << 8);
    }

    // ---------------------------------------------------------------
    // TSEG / SMRAM (from memmap.c)
    // ---------------------------------------------------------------

    /// Decode TSEG size from ESMRAMC register (bytes).
    ///
    /// Returns 0 if T_EN (bit 0) is not set.
    pub fn tseg_size(&self) -> u32 {
        let esmramc = ecam::read8(0, 0, 0, hostbridge::ESMRAMC);
        if esmramc & 1 == 0 {
            return 0;
        }
        match (esmramc >> 1) & 3 {
            0 => 1 * 1024 * 1024, // 1 MiB
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
        ecam::read32(0, 0, 0, hostbridge::TSEG)
    }

    /// Get the SMM region (base + size) as a `(base, size)` pair.
    ///
    /// Used by the MP init code to know where TSEG lives.
    pub fn smm_region(&self) -> (u32, u32) {
        (self.tseg_base(), self.tseg_size())
    }

    /// Compute CBMEM top (aligned down to 4 MiB).
    ///
    /// TSEG can start at any 1 MiB alignment; CBMEM needs 4 MiB
    /// alignment for MTRR efficiency.
    pub fn cbmem_top(&self) -> u32 {
        self.tseg_base() & !((4 * 1024 * 1024) - 1)
    }

    /// Write the SMRAM register (used by SMM relocation).
    pub fn write_smram(&self, val: u8) {
        ecam::write8(0, 0, 0, hostbridge::SMRAM, val);
    }

    /// Read the SMRAM register.
    pub fn read_smram(&self) -> u8 {
        ecam::read8(0, 0, 0, hostbridge::SMRAM)
    }

    // ---------------------------------------------------------------
    // Full memory map (from northbridge.c)
    // ---------------------------------------------------------------

    /// Read the graphics stolen memory base (GBSM register).
    pub fn igd_base(&self) -> u32 {
        ecam::read32(0, 0, 0, hostbridge::GBSM)
    }

    /// Read the GTT stolen memory base (BGSM register).
    pub fn gtt_base(&self) -> u32 {
        ecam::read32(0, 0, 0, hostbridge::BGSM)
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
        fstart_log::info!("pineview: CBMEM top={:#x}", self.cbmem_top());
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
            let name = config.acpi_name.as_deref().unwrap_or("MCHC");
            let _adr: u32 = 0x0000_0000;
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;

            // 1. MCHC device with PCI config OperationRegion.
            //
            // Coreboot hostbridge.asl: MCHP OpRegion in PCI_Config
            // with fields for EPBAR, MCHBAR, PCIEXBAR, DMIBAR, PAM
            // registers, TOM, and TOLUD.  These are read by the OS
            // to discover memory topology.
            let mut aml = fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_ADR", #{_adr});

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
            };

            // 2. PDRC — Platform Device Resource Consumption.
            //
            // Reserves MMIO ranges for RCBA, MCHBAR, DMIBAR, EPBAR,
            // and miscellaneous ICH regions so the OS won't allocate
            // PCI BARs over them.
            // Coreboot: pineview.asl Device(PDRC)
            let rcba: u32 = 0xFED1_C000; // ICH7 default RCBA
            aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
                Device("PDRC") {
                    Name("_HID", EisaId("PNP0C02"));
                    Name("_UID", 1u32);
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{rcba}, 0x4000u32);
                        Memory32Fixed(ReadWrite, #{mchbar}, 0x4000u32);
                        Memory32Fixed(ReadWrite, #{dmibar}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{epbar}, 0x1000u32);
                        // Misc ICH MMIO (HPET area, TPM, etc.)
                        Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                        Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                        Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                    });
                }
            });

            // 3. PCI0 host bridge identity.
            //
            // _HID PNP0A08 (PCIe), _CID PNP0A03 (PCI), _BBN 0.
            // These tell the OS this is the root PCI host bridge.
            // Coreboot: hostbridge.asl top-level Names.
            aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
                Name("_HID", EisaId("PNP0A08"));
                Name("_CID", EisaId("PNP0A03"));
                Name("_BBN", 0u32);
            });

            aml
        }

        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }
}
