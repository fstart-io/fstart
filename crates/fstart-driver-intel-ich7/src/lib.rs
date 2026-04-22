//! Intel ICH7 (I/O Controller Hub 7) southbridge driver.
//!
//! Applies to the NM10 Express used on Atom D-series boards and the
//! generic ICH7 found on Core 2 / Pentium 4 platforms. Responsibilities:
//!
//! - Program RCBA so the chipset's MMIO block is addressable.
//! - Open LPC I/O decode windows for the SuperIO UART and boot ROM.
//! - Program PIRQ routing.
//! - Apply the function-disable mask.
//! - Enable the I801 SMBus controller.
//!
//! All PCI config access goes through ECAM MMIO (the Pineview
//! `early_init` has already enabled PCIEXBAR before this driver runs).

#![no_std]

use fstart_pineview_regs::{ecam, ich7, Rcba};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{ServiceError, SmBus, Southbridge};
use fstart_smbus_intel::I801SmBus;
use fstart_superio::LpcBaseProvider;
use serde::{Deserialize, Serialize};

// Re-export HDA types from the shared crate so board RON configs
// can reference them via the ICH7 driver path.
pub use fstart_hda::{HdaConfig, HdaController, HdaVerbTable};

// ---------------------------------------------------------------------------
// ICH7 LPC PCI config register offsets (bus 0, dev 0x1f, func 0)
// ---------------------------------------------------------------------------

const SMLT: u16 = 0x1B;
const SERIRQ_CNTL: u16 = 0x64;
const GEN_PMCON_3: u16 = 0xA4;
const RTC_BATTERY_DEAD: u8 = 1 << 2;
const ACPI_CNTL: u16 = 0x44;
const ACPI_EN: u8 = 0x80;
const GPIO_CNTL: u16 = 0x4C;
const GPIO_EN: u8 = 0x10;
const LPC_IO_DEC: u16 = 0x80;
const LPC_EN: u16 = 0x82;
const GEN1_DEC: u16 = 0x84;
const GEN2_DEC: u16 = 0x88;
const GEN3_DEC: u16 = 0x8C;
const GEN4_DEC: u16 = 0x90;
const PMBASE_REG: u16 = 0x40;
const GPIOBASE_REG: u16 = 0x48;

const DEFAULT_PMBASE: u32 = 0x0500;
const DEFAULT_GPIOBASE: u32 = 0x0480;

/// LPC_EN bits: enable all standard decode ranges.
///  CNF2 (0x4e) | CNF1 (0x2e) | MC (0x62) | KBC (0x60) |
///  GAMEH | GAMEL | FDD | LPT | COMB | COMA
const LPC_EN_ALL: u16 = (1 << 13)
    | (1 << 12)
    | (1 << 11)
    | (1 << 10)
    | (1 << 9)
    | (1 << 8)
    | (1 << 3)
    | (1 << 2)
    | (1 << 1)
    | (1 << 0);

/// RCBA offset: OIC (Other Interrupt Control — IOAPIC enable).
const OIC: u32 = 0x31FE;
/// RCBA offset: HPET Configuration register.
const HPTC: u32 = 0x3404;
/// HPET base address (after HPTC enable).
const HPET_BASE: usize = 0xFED0_0000;
/// PM1_CNT register (offset from PMBASE).
const PM1_CNT_OFFSET: u16 = 0x04;
/// SLP_TYP mask in PM1_CNT [12:10].
const SLP_TYP_MASK: u32 = 0x1C00;
/// S3 (STR) SLP_TYP value.
const SLP_TYP_S3: u32 = 0x1400;

// GPIO register offsets from GPIOBASE.
const GPIO_USE_SEL: u16 = 0x00;
const GP_IO_SEL: u16 = 0x04;
const GP_LVL: u16 = 0x0C;
const GPO_BLINK: u16 = 0x18;
const GPI_INV: u16 = 0x2C;
const GPIO_USE_SEL2: u16 = 0x30;
const GP_IO_SEL2: u16 = 0x34;
const GP_LVL2: u16 = 0x38;
const GP_RST_SEL1: u16 = 0x60;
const GP_RST_SEL2: u16 = 0x64;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// SATA configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SataConfig {
    pub mode: SataMode,
    pub ports: u8,
}

/// SATA controller operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SataMode {
    Ide,
    Ahci,
}

/// USB controller configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UsbConfig {
    #[serde(default)]
    pub ehci: bool,
    #[serde(default)]
    pub uhci: [bool; 4],
}

// HDA verb table types are defined in the shared fstart-hda crate.
// See fstart_hda::{hda_verb, hda_pin_cfg, hda_pin_nc} for helpers.

/// GPIO pad configuration for one set of 32 pins.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GpioSet {
    /// GPIO mode: 0 = native, 1 = GPIO. Bits correspond to GPIO pins.
    #[serde(default)]
    pub mode: u32,
    /// Direction: 0 = output, 1 = input (only for pins in GPIO mode).
    #[serde(default)]
    pub direction: u32,
    /// Output level: 0 = low, 1 = high.
    #[serde(default)]
    pub level: u32,
    /// Blink enable (set 1 only).
    #[serde(default)]
    pub blink: u32,
    /// Input inversion (set 1 only).
    #[serde(default)]
    pub invert: u32,
    /// Reset select.
    #[serde(default)]
    pub reset: u32,
}

/// ICH7 southbridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelIch7Config {
    /// Root Complex Base Address register value.
    pub rcba: u64,
    /// PIRQ routing (one byte per PIRQ A..H).
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits.
    pub gpe0_en: u32,
    /// LPC I/O decode register values (GEN1..GEN4).
    pub lpc_decode: [u32; 4],
    /// HD Audio (Azalia) configuration with verb tables.
    #[serde(default)]
    pub hda: Option<HdaConfig>,
    /// SATA configuration.
    #[serde(default)]
    pub sata: Option<SataConfig>,
    /// USB configuration.
    #[serde(default)]
    pub usb: Option<UsbConfig>,
    /// Enable PATA (legacy IDE) function.
    #[serde(default)]
    pub pata: bool,
    /// ECAM base address (must match the Pineview NB config).
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// SMBus I/O base address.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// GPIO set 1 (pins 0..31).
    #[serde(default)]
    pub gpio_set1: GpioSet,
    /// GPIO set 2 (pins 32..63).
    #[serde(default)]
    pub gpio_set2: GpioSet,
    /// ACPI device name (e.g., "LPCB"). If `None`, no ACPI node.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
    /// C3 latency in microseconds (for FADT p_lvl3_lat).
    #[serde(default = "default_c3_latency")]
    pub c3_latency: u16,
    /// After-power-failure behaviour: 0=off, 1=on, 2=last-state.
    #[serde(default)]
    pub power_on_after_fail: u8,
}

fn default_c3_latency() -> u16 {
    85 // typical ICH7 value
}

fn default_ecam_base() -> u64 {
    0xE000_0000
}

fn default_smbus_base() -> u16 {
    ich7::DEFAULT_SMBUS_BASE
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

/// Intel ICH7 southbridge driver.
pub struct IntelIch7 {
    config: IntelIch7Config,
    /// I801 SMBus controller, initialised during `early_init`.
    smbus: Option<I801SmBus>,
}

// SAFETY: All state is CPU-exclusive during firmware phase.
unsafe impl Send for IntelIch7 {}
unsafe impl Sync for IntelIch7 {}

impl IntelIch7 {
    /// ECAM accessor.

    /// CIR (Chipset Initialization Registers) magic writes.
    ///
    /// Ported from coreboot `ich7_setup_cir()`.
    fn setup_cir(&self, rcba: &Rcba) {
        rcba.write32(0x0088, 0x0011_D000);
        rcba.write16(0x01FC, 0x060F);
        rcba.write32(0x01F4, 0x8600_0040);
        // Bit 6 is set but not read back.
        rcba.write32(0x0214, 0x1003_0549);
        rcba.write32(0x0218, 0x0002_0504);
        rcba.write8(0x0220, 0xC5);
        // RCBA 0x3430: clear bits [1:0], set bit 0.
        let v = rcba.read32(0x3430);
        rcba.write32(0x3430, (v & !3) | 1);

        rcba.write16(0x0200, 0x2008);
        rcba.write8(0x2027, 0x0D);

        // PCIe link tuning.
        let v = rcba.read16(0x3E08);
        rcba.write16(0x3E08, v | (1 << 7));
        let v = rcba.read16(0x3E48);
        rcba.write16(0x3E48, v | (1 << 7));
        let v = rcba.read32(0x3E0E);
        rcba.write32(0x3E0E, v | (1 << 7));
        let v = rcba.read32(0x3E4E);
        rcba.write32(0x3E4E, v | (1 << 7));

        // Mobile variant fixup: check PCI device ID.
        let pci_id = ecam::read16(0, ich7::LPC_DEV, ich7::LPC_FUNC, 0x02);
        match pci_id {
            0x27B9 | 0x27BC | 0x27BD => {
                let rev = ecam::read8(0, ich7::LPC_DEV, ich7::LPC_FUNC, 0x08);
                if rev >= 2 {
                    let v = rcba.read32(0x2034);
                    rcba.write32(0x2034, (v & !(0x0F << 16)) | (5 << 16));
                }
                // FERR# MUX Enable.
                let gcs = rcba.read32(ich7::GCS);
                rcba.write32(ich7::GCS, gcs | (1 << 6));
            }
            _ => {}
        }

        fstart_log::info!("intel-ich7: CIR configured (pci_id={:#06x})", pci_id);
    }

    /// Program GPIO pads via GPIOBASE I/O ports.
    ///
    /// Ported from coreboot `setup_pch_gpios()`.
    #[cfg(target_arch = "x86_64")]
    fn setup_gpios(&self) {
        let base = DEFAULT_GPIOBASE as u16;
        let g1 = &self.config.gpio_set1;
        let g2 = &self.config.gpio_set2;

        // SAFETY: GPIOBASE was programmed above and is a valid I/O range.
        unsafe {
            // Set 1 — order matters on ICH7: level first, then mode/direction,
            // then level again to avoid glitches.
            fstart_pio::outl(base + GP_LVL, g1.level);
            fstart_pio::outl(base + GPIO_USE_SEL, g1.mode);
            fstart_pio::outl(base + GP_IO_SEL, g1.direction);
            fstart_pio::outl(base + GP_LVL, g1.level);
            fstart_pio::outl(base + GP_RST_SEL1, g1.reset);
            fstart_pio::outl(base + GPI_INV, g1.invert);
            fstart_pio::outl(base + GPO_BLINK, g1.blink);

            // Set 2.
            fstart_pio::outl(base + GP_LVL2, g2.level);
            fstart_pio::outl(base + GPIO_USE_SEL2, g2.mode);
            fstart_pio::outl(base + GP_IO_SEL2, g2.direction);
            fstart_pio::outl(base + GP_LVL2, g2.level);
            fstart_pio::outl(base + GP_RST_SEL2, g2.reset);
        }

        fstart_log::info!("intel-ich7: GPIOs configured");
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn setup_gpios(&self) {
        fstart_log::info!("intel-ich7: GPIO setup (stub, non-x86)");
    }

    /// Enable HPET via RCBA.
    ///
    /// Ported from coreboot `enable_hpet()`. Raminit needs HPET for
    /// microsecond-resolution delays (hpet_udelay).
    fn enable_hpet(&self, rcba: &Rcba) {
        let v = rcba.read32(HPTC);
        rcba.write32(HPTC, (v & !0x03) | (1 << 7));
        // Read back for posted write.
        let _ = rcba.read32(HPTC);

        // Enable the main HPET counter.
        // SAFETY: HPET base is a fixed MMIO address enabled by HPTC.
        unsafe {
            let cfg = fstart_mmio::read32((HPET_BASE + 0x10) as *const u32);
            fstart_mmio::write32((HPET_BASE + 0x10) as *mut u32, cfg | 1);
        }

        fstart_log::info!("intel-ich7: HPET enabled at {:#x}", HPET_BASE);
    }

    /// Detect S3 resume from PM1_CNT SLP_TYP field.
    ///
    /// Ported from coreboot `southbridge_detect_s3_resume()`.
    #[cfg(target_arch = "x86_64")]
    fn detect_s3_resume(&self) -> bool {
        // SAFETY: PMBASE is a valid I/O base programmed during early_init.
        let pm1_cnt = unsafe { fstart_pio::inl(DEFAULT_PMBASE as u16 + PM1_CNT_OFFSET) };
        let slp_typ = pm1_cnt & SLP_TYP_MASK;
        if slp_typ == SLP_TYP_S3 {
            // Clear SLP_TYP so we don't re-detect on warm reset.
            unsafe {
                fstart_pio::outl(
                    DEFAULT_PMBASE as u16 + PM1_CNT_OFFSET,
                    pm1_cnt & !SLP_TYP_MASK,
                );
            }
            fstart_log::info!("intel-ich7: S3 resume detected");
            true
        } else {
            false
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn detect_s3_resume(&self) -> bool {
        false
    }

    /// Compute the Function Disable (FD) bitmask.
    fn function_disable_mask(&self) -> u32 {
        let mut fd = 0u32;
        if self.config.hda.is_none() {
            fd |= 1 << 4;
        }
        if self.config.sata.is_none() {
            fd |= 1 << 2;
        }
        if !self.config.pata {
            fd |= 1 << 1;
        }
        fd
    }
}

impl Device for IntelIch7 {
    const NAME: &'static str = "intel-ich7";
    const COMPATIBLE: &'static [&'static str] = &["intel,ich7", "intel,nm10"];
    type Config = IntelIch7Config;

    fn new(config: &IntelIch7Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config: config.clone(),
            smbus: None,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        fstart_log::info!("intel-ich7: rcba={:#x}", self.config.rcba);
        Ok(())
    }
}

impl Southbridge for IntelIch7 {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // ---- 0. SPI prefetch + upper CMOS (bootblock-level on coreboot) ----
        // On coreboot these run from bootblock_early_southbridge_init().
        // We do them here since fstart has a single early_init path.
        //
        // SPI prefetch/caching: LPC reg 0xDC bits [3:2] = 10 (enable prefetch).
        let spi = ecam::read8(0, d, f, 0xDC);
        ecam::write8(0, d, f, 0xDC, (spi & !(3 << 2)) | (2 << 2));

        // ---- 1. Enable SMBus (must be first — raminit reads SPD) ----
        let smbus = I801SmBus::enable_on_ich7(self.config.smbus_base);
        self.smbus = Some(smbus);

        // ---- 2. Setup BARs ----
        // RCBA
        ecam::write32(
            0,
            d,
            f,
            ich7::RCBA_REG,
            (self.config.rcba as u32 & 0xFFFF_C000) | 1,
        );
        // PMBASE + ACPI enable
        ecam::write32(0, d, f, PMBASE_REG, DEFAULT_PMBASE | 1);
        ecam::write8(0, d, f, ACPI_CNTL, ACPI_EN);
        // GPIOBASE + GPIO enable
        ecam::write32(0, d, f, GPIOBASE_REG, DEFAULT_GPIOBASE | 1);
        ecam::write8(0, d, f, GPIO_CNTL, GPIO_EN);

        // ---- 3. Serial IRQ configuration ----
        ecam::write8(0, d, f, SERIRQ_CNTL, 0xD0);

        // ---- 4. LPC I/O decode and enable ----
        ecam::write16(0, d, f, LPC_IO_DEC, 0x0010);
        // Enable: SuperIO (CNF1/CNF2), KBC, COMA, COMB, LPT, FDD, GAME
        ecam::write16(0, d, f, LPC_EN, LPC_EN_ALL);
        // Generic decode ranges (GEN1..GEN4) from board config.
        ecam::write32(0, d, f, GEN1_DEC, self.config.lpc_decode[0]);
        ecam::write32(0, d, f, GEN2_DEC, self.config.lpc_decode[1]);
        ecam::write32(0, d, f, GEN3_DEC, self.config.lpc_decode[2]);
        ecam::write32(0, d, f, GEN4_DEC, self.config.lpc_decode[3]);

        // ---- 5. PIRQ routing ----
        let pirq_low = u32::from_le_bytes([
            self.config.pirq_routing[0],
            self.config.pirq_routing[1],
            self.config.pirq_routing[2],
            self.config.pirq_routing[3],
        ]);
        let pirq_high = u32::from_le_bytes([
            self.config.pirq_routing[4],
            self.config.pirq_routing[5],
            self.config.pirq_routing[6],
            self.config.pirq_routing[7],
        ]);
        ecam::write32(0, d, f, 0x60, pirq_low);
        ecam::write32(0, d, f, 0x68, pirq_high);

        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);

        // Enable upper 128 bytes of CMOS.
        rcba.write32(0x3400, 1 << 2);

        // ---- 6. Disable watchdog reboot ----
        rcba.write32(ich7::GCS, rcba.read32(ich7::GCS) | (1 << 5));
        // Halt TCO timer, clear timeout status.
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: PMBASE is a valid I/O base programmed above.
            unsafe {
                let pm = DEFAULT_PMBASE as u16;
                let tco = pm + 0x60;
                let v = fstart_pio::inw(tco + 0x08);
                fstart_pio::outw(tco + 0x08, v | (1 << 11));
                fstart_pio::outw(tco + 0x04, 1 << 3);
                fstart_pio::outw(tco + 0x06, 1 << 1);
            }
        }

        // ---- 7. PCI bridge secondary MLT ----
        ecam::write8(0, 0x1e, 0, SMLT, 0x20);

        // ---- 8. Reset RTC power status ----
        ecam::and8(0, d, f, GEN_PMCON_3, !RTC_BATTERY_DEAD);

        // ---- 9. USB pre-config ----
        ecam::or8(0, d, f, 0xAD, 3);
        ecam::or32(0, 0x1d, 7, 0xFC, (1 << 29) | (1 << 17));
        ecam::or32(0, 0x1d, 7, 0xDC, (1 << 31) | (1 << 27));

        // ---- 10. Enable IOAPIC ----
        rcba.write8(OIC, 0x03);
        let _ = rcba.read8(OIC); // flush

        // ---- 11. CIR (Chipset Initialization Registers) ----
        self.setup_cir(&rcba);

        // ---- 12. Function disable mask ----
        let fd = self.function_disable_mask();
        rcba.write32(0x3418, fd);

        // ---- 13. GPIO pad programming ----
        self.setup_gpios();

        // ---- 14. Enable HPET (needed by raminit for hpet_udelay) ----
        self.enable_hpet(&rcba);

        fstart_log::info!("intel-ich7: early init complete (fd_mask={:#x})", fd);
        Ok(())
    }
}

impl IntelIch7 {
    /// Detect boot path: Normal, Reset (warm), or S3 Resume.
    ///
    /// Call this after `early_init()` but before raminit to determine
    /// which raminit steps to skip.
    pub fn detect_boot_path(&self) -> u8 {
        if self.detect_s3_resume() {
            return 2; // BOOT_PATH_RESUME
        }
        // Check MCHBAR PMSTS bit 8 for warm reset (done by NB driver).
        // The SB just checks PM1_CNT.
        0 // BOOT_PATH_NORMAL
    }
}

impl SmBus for IntelIch7 {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.read_byte(addr, cmd),
            None => Err(ServiceError::HardwareError),
        }
    }
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.write_byte(addr, cmd, value),
            None => Err(ServiceError::HardwareError),
        }
    }
}

impl LpcBaseProvider for IntelIch7 {
    fn lpc_base(&self) -> u16 {
        0x2e
    }
}

// ---------------------------------------------------------------------------
// Ramstage init — ported from coreboot lpc.c / sata.c / usb.c / i82801gx.c
// ---------------------------------------------------------------------------

impl IntelIch7 {
    /// Late initialisation, called from the ramstage after DRAM is online.
    ///
    /// Ported from coreboot's ICH7 ramstage device ops: `lpc_init`,
    /// `sata_init`, `usb_init`, `usb_ehci_init`, `enable_clock_gating`,
    /// `lpc_final`.
    pub fn ramstage_init(&self) -> Result<(), ServiceError> {
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // ---- SATA ----
        if let Some(ref sata) = self.config.sata {
            self.sata_init(sata);
        }

        // ---- USB UHCI (bus master + errata) ----
        if let Some(ref usb) = self.config.usb {
            self.usb_uhci_init(usb);
            if usb.ehci {
                self.usb_ehci_init();
            }
        }

        // ---- Power management ----
        self.power_management_init();

        // ---- C-state configuration ----
        // Popup & Popdown enable, Deeper Sleep timings.
        ecam::or8(0, d, f, 0xA9, (1 << 4) | (1 << 3) | (1 << 2));
        let v = ecam::read8(0, d, f, 0xAA);
        ecam::write8(0, d, f, 0xAA, (v & 0xF0) | (2 << 2) | 2);

        // ---- Clock gating ----
        self.enable_clock_gating();

        // ---- ISA DMA controller reset ----
        self.isa_dma_init();

        // ---- i8259 PIC init ----
        self.i8259_init();

        // ---- GPE0 enable + PM1_CNT (SCI, bus master C3->C0) ----
        self.power_options_late();

        // ---- SPI access request clear ----
        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);
        let spi_ctrl = rcba.read16(0x3802);
        rcba.write16(0x3802, spi_ctrl & !(1u16));

        // ---- PCIe root port init ----
        self.pcie_init();

        // ---- HD Audio (Azalia) init ----
        if let Some(ref hda) = self.config.hda {
            self.hda_init(hda);
        }

        // ---- USB Transient Disconnect Detect (fixup) ----
        ecam::write8(0, d, f, 0xAD, 0x03);

        // ---- RCBA fixup (must be after PCI enumeration) ----
        rcba.write32(0x1D40, rcba.read32(0x1D40) | 1);

        // ---- Disable performance counter (RCBA FD bit 0) ----
        rcba.write32(0x3418, rcba.read32(0x3418) | 1);

        fstart_log::info!("intel-ich7: ramstage init complete");
        Ok(())
    }

    /// SATA controller initialisation.
    ///
    /// Ported from coreboot `sata_init()`. Programs the SATA controller
    /// into AHCI or IDE mode and runs the mandatory init sequence.
    fn sata_init(&self, sata: &SataConfig) {
        let d: u8 = 0x1f;
        let f: u8 = 2;

        // Enable BARs.
        ecam::or16(0, d, f, 0x04, 0x07); // IO + Mem + BusMaster

        match sata.mode {
            SataMode::Ahci => {
                fstart_log::info!("intel-ich7: SATA in AHCI mode");
                // Map = AHCI.
                let v = ecam::read8(0, d, f, 0x90);
                ecam::write8(0, d, f, 0x90, (v & !0xC3) | 0x40);
                // Native mode on both channels.
                ecam::write8(0, d, f, 0x09, 0x8F);
                // Interrupt line.
                ecam::write8(0, d, f, 0x3C, 0x0A);
            }
            SataMode::Ide => {
                fstart_log::info!("intel-ich7: SATA in IDE mode");
                ecam::write8(0, d, f, 0x90, ecam::read8(0, d, f, 0x90) & !0xC3);
                ecam::write8(0, d, f, 0x09, 0x8F);
                ecam::write8(0, d, f, 0x3C, 0xFF);
                // IDE timings.
                ecam::write16(0, d, f, 0x40, 0xB301); // PRI
                ecam::write16(0, d, f, 0x42, 0xB301); // SEC
                ecam::write16(0, d, f, 0x48, 0x0005); // Sync DMA cnt
                ecam::write16(0, d, f, 0x4A, 0x0201); // Sync DMA tim
                ecam::write32(0, d, f, 0x54, 0x00000033); // IDE I/O cfg
            }
        }

        // Port control.
        ecam::write8(0, d, f, 0x92, sata.ports);

        // Clock gating + init register.
        let ports = sata.ports;
        let sif3: u32 = match ports {
            0x0F => 0,
            0x03 => 1 << 24,
            0x01 => (1 << 24) | (1 << 20),
            _ => 0,
        };
        ecam::write32(0, d, f, 0x94, sif3 | (1 << 16) | (1 << 18) | (1 << 19));

        // Mandatory SATA init sequence (from coreboot).
        ecam::write8(0, d, f, 0xA0, 0x40);
        ecam::write8(0, d, f, 0xA6, 0x22);
        ecam::write8(0, d, f, 0xA0, 0x78);
        ecam::write8(0, d, f, 0xA6, 0x22);
        ecam::write8(0, d, f, 0xA0, 0x88);
        let v = ecam::read32(0, d, f, 0xA4);
        ecam::write32(0, d, f, 0xA4, (v & 0xC0C0_C0C0) | 0x1B1B_1212);
        ecam::write8(0, d, f, 0xA0, 0x8C);
        let v = ecam::read32(0, d, f, 0xA4);
        ecam::write32(0, d, f, 0xA4, (v & 0xC0C0_FF00) | 0x1212_00AA);
        ecam::write8(0, d, f, 0xA0, 0x00);
        ecam::write8(0, d, f, 0x3C, 0x00);
        ecam::or32(0, d, f, 0x94, 1 << 22); // SCRD due to bug

        fstart_log::info!("intel-ich7: SATA init done (ports={:#x})", ports);
    }

    /// UHCI (USB 1.1) controller init.
    fn usb_uhci_init(&self, usb: &UsbConfig) {
        for (i, &enabled) in usb.uhci.iter().enumerate() {
            if !enabled {
                continue;
            }
            let f = i as u8; // UHCI #1..#4 are dev 29, func 0..3
                             // Bus master.
            ecam::or16(0, 0x1D, f, 0x04, 0x04);
            // Errata workarounds.
            ecam::write8(0, 0x1D, f, 0xCA, 0x00);
            ecam::or8(0, 0x1D, f, 0xCA, 1);
        }
        fstart_log::info!("intel-ich7: UHCI init done");
    }

    /// EHCI (USB 2.0) controller init.
    fn usb_ehci_init(&self) {
        // Bus master + SERR.
        ecam::or16(0, 0x1D, 7, 0x04, 0x06);
        // Debug port + async schedule park.
        ecam::or32(0, 0x1D, 7, 0xDC, (1 << 31) | (1 << 27));
        let v = ecam::read32(0, 0x1D, 7, 0xFC);
        ecam::write32(
            0,
            0x1D,
            7,
            0xFC,
            (v & !(3 << 2)) | (2 << 2) | (1 << 29) | (1 << 17),
        );
        // Errata.
        ecam::or8(0, 0x1D, 7, 0x84, 1 << 4);
        fstart_log::info!("intel-ich7: EHCI init done");
    }

    /// Power management init (from coreboot `i82801gx_power_options`).
    fn power_management_init(&self) {
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // Power-on after failure.
        let mut pmcon3 = ecam::read8(0, d, f, GEN_PMCON_3);
        pmcon3 |= 3 << 4; // avoid #S4 assertions
        pmcon3 &= !(1 << 3); // minimum assertion 1-2 RTCCLK
        match self.config.power_on_after_fail {
            0 => pmcon3 |= 1,  // stay off
            _ => pmcon3 &= !1, // power on / last state
        }
        ecam::write8(0, d, f, GEN_PMCON_3, pmcon3);

        // GEN_PMCON_1: SMI rate, SpeedStep, CPUSLP, BIOS_PCI_EXP.
        let mut pmcon1 = ecam::read16(0, d, f, 0xA0);
        pmcon1 &= !3; // SMI rate 1 minute
        pmcon1 |= (1 << 5)      // CPUSLP_EN
                | (1 << 10); // BIOS_PCI_EXP_EN
        ecam::write16(0, d, f, 0xA0, pmcon1);

        fstart_log::info!("intel-ich7: power management configured");
    }

    /// Enable clock gating (from coreboot `enable_clock_gating`).
    fn enable_clock_gating(&self) {
        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);
        let mut cg = rcba.read32(0x341C);
        cg |= (1 << 31)  // LPC
            | (1 << 30)   // PATA
            | (1 << 27) | (1 << 26) | (1 << 25) | (1 << 24)  // SATA
            | (1 << 23)   // AC97
            | (1 << 19)   // EHCI
            | (1 << 3) | (1 << 1)  // DMI
            | (1 << 2); // PCIe
        cg &= !(1 << 20); // no static USB clock gating
        cg &= !((1 << 29) | (1 << 28)); // disable UHCI clock gating
        rcba.write32(0x341C, cg);
        fstart_log::info!("intel-ich7: clock gating enabled");
    }

    /// ISA DMA controller initialization.
    ///
    /// Programs the 8237 DMA controller into known state. Standard
    /// x86 POST sequence — ensures DMA channels are masked and the
    /// controller is in a known operating mode.
    fn isa_dma_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use fstart_pio::{inb, outb};

            // DMA controller 1 (channels 0-3)
            outb(0x0D, 0x00); // Master clear
            outb(0x0B, 0x40); // Channel 0: single, addr increment, demand
            outb(0x0B, 0x41); // Channel 1
            outb(0x0B, 0x42); // Channel 2
            outb(0x0B, 0x43); // Channel 3

            // DMA controller 2 (channels 4-7)
            outb(0xDA, 0x00); // Master clear
            outb(0xD6, 0xC0); // Channel 4: cascade mode
            outb(0xD6, 0x41); // Channel 5
            outb(0xD6, 0x42); // Channel 6
            outb(0xD6, 0x43); // Channel 7

            // Unmask DMA controller 2 channel 4 (cascade).
            outb(0xD4, 0x00);
            // Mask all DMA controller 1 channels.
            outb(0x0F, 0x0F);

            let _ = inb(0x80); // small delay
        }
    }

    /// i8259 PIC initialization.
    ///
    /// Sets up the dual 8259 interrupt controllers in the standard
    /// PC/AT configuration.  IRQ 9 is configured as level-triggered
    /// for ACPI SCI.
    fn i8259_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use fstart_pio::outb;

            // ICW1: begin init, ICW4 needed.
            outb(0x20, 0x11); // Master PIC
            outb(0xA0, 0x11); // Slave PIC
                              // ICW2: vector offset (master=0x08, slave=0x70).
            outb(0x21, 0x08);
            outb(0xA1, 0x70);
            // ICW3: master has slave on IRQ2, slave ID=2.
            outb(0x21, 0x04);
            outb(0xA1, 0x02);
            // ICW4: 8086 mode.
            outb(0x21, 0x01);
            outb(0xA1, 0x01);
            // Mask all interrupts.
            outb(0x21, 0xFF);
            outb(0xA1, 0xFF);

            // Set IRQ 9 as level-triggered (ACPI SCI).
            // ELCR1 (port 0x4D0) and ELCR2 (port 0x4D1).
            let elcr2 = fstart_pio::inb(0x4D1);
            fstart_pio::outb(0x4D1, elcr2 | (1 << 1)); // IRQ 9 = bit 1 of ELCR2
        }
    }

    /// Late power options: GPE0 enable, NMI control, PM1_CNT.
    ///
    /// Completes the power management setup started in early_init.
    /// Programs GPE0_EN from the board config, sets PM1_CNT for
    /// SCI_EN and bus-master C3->C0 wakeup, configures NMI source
    /// control.
    fn power_options_late(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use fstart_pio::{inb, inl, outb, outl, outw};

            let pm = DEFAULT_PMBASE as u16;

            // GPE0_EN from board config.
            outl(pm + 0x2C, self.config.gpe0_en);

            // NMI source control (port 0x61).
            let mut nmi = inb(0x61);
            nmi &= 0x0F; // Upper nibble must be 0
            nmi |= 1 << 2; // PCI SERR# disable (for now)
            nmi &= !(1 << 3); // IOCHK# NMI enable
            outb(0x61, nmi);

            // Disable NMI sources (port 0x70 bit 7).
            let nmi_ctl = inb(0x70);
            outb(0x70, nmi_ctl | (1 << 7));

            // PM1_CNT: enable SCI, bus-master C3->C0 wakeup.
            let mut pm1 = inl(pm + 0x04);
            pm1 &= !0x1C00; // Clear SLP_TYP
            pm1 |= 1 << 1; // BM_RLD: bus master C3->C0
            pm1 |= 1 << 0; // SCI_EN
            outl(pm + 0x04, pm1);
        }
    }

    /// PCIe root port initialization.
    ///
    /// Programs all enabled PCIe root ports (dev 0x1C, func 0..5)
    /// with bus master, cache line size, clock gating, VC0 traffic
    /// class, and common clock configuration.
    ///
    /// Ported from coreboot `pcie.c::pci_init()`.
    fn pcie_init(&self) {
        // ICH7 has up to 6 PCIe root ports at dev 0x1C func 0..5.
        for func in 0u8..6 {
            let vid = ecam::read16(0, 0x1C, func, 0x00);
            if vid == 0xFFFF {
                continue; // Function not present
            }

            // Enable bus master.
            ecam::or16(0, 0x1C, func, 0x04, 0x07); // IO+Mem+BusMaster

            // Cache line size = 0x10.
            ecam::write8(0, 0x1C, func, 0x0C, 0x10);

            // Disable parity error response on bridge control.
            ecam::and16(0, 0x1C, func, 0x3E, !1u16);

            // Enable IO xAPIC on this port.
            ecam::or32(0, 0x1C, func, 0xD8, 1 << 7);

            // Enable backbone clock gating.
            ecam::or32(0, 0x1C, func, 0xE1, 0x0F);

            // VC0 traffic class.
            let vc0 = ecam::read32(0, 0x1C, func, 0x114);
            ecam::write32(0, 0x1C, func, 0x114, (vc0 & !0xFF) | 1);

            // Mask completion timeouts.
            ecam::or32(0, 0x1C, func, 0x148, 1 << 14);

            // Enable common clock configuration.
            ecam::or16(0, 0x1C, func, 0x50, 1 << 6);

            fstart_log::info!("intel-ich7: PCIe port {} init", func);
        }
    }

    /// HD Audio (Azalia) controller initialization.
    ///
    /// Enables the HDA controller at dev 0x1B func 0, performs codec
    /// discovery via the STATESTS register, and programs verb tables
    /// from the board configuration.
    ///
    /// Ported from coreboot `azalia.c::azalia_init()`.
    fn hda_init(&self, hda: &HdaConfig) {
        let d: u8 = 0x1B;
        let f: u8 = 0;

        let vid = ecam::read16(0, d, f, 0x00);
        if vid == 0xFFFF {
            fstart_log::info!("intel-ich7: HDA not present");
            return;
        }

        // ESD fix.
        let esd = ecam::read32(0, d, f, 0x134);
        ecam::write32(0, d, f, 0x134, (esd & !(0xFF << 16)) | (2 << 16));

        // Link1 description.
        let l1 = ecam::read32(0, d, f, 0x140);
        ecam::write32(0, d, f, 0x140, (l1 & !(0xFF << 16)) | (2 << 16));

        // VC0 resource control.
        let vc0 = ecam::read32(0, d, f, 0x114);
        ecam::write32(0, d, f, 0x114, (vc0 & !0xFF) | 1);

        // VCi traffic class (TC7).
        ecam::or8(0, d, f, 0x44, 7);

        // VCi resource control: enable, ID, TC mapping.
        ecam::or32(0, d, f, 0x120, (1 << 31) | (1 << 24) | 0x80);

        // Enable bus master.
        ecam::or16(0, d, f, 0x04, 0x06); // Mem + BusMaster

        // Clock detect cycle.
        ecam::or8(0, d, f, 0x40, 1 << 3); // Set CLKDETCLR
        ecam::and8(0, d, f, 0x40, !(1 << 3)); // Clear it
        ecam::or8(0, d, f, 0x40, 1 << 2); // Enable clock detection

        // Select Azalia mode.
        ecam::or8(0, d, f, 0x40, 1);

        // Disable docking.
        ecam::and8(0, d, f, 0x4D, !(1 << 7));

        // Read BAR0 for MMIO base.
        let bar = ecam::read32(0, d, f, 0x10) & !0xF;
        if bar == 0 {
            fstart_log::error!("intel-ich7: HDA BAR0 not assigned");
            return;
        }
        let base = bar as usize;

        // Use the shared HDA controller for codec detection + verb programming.
        let hda_ctrl = HdaController::new(base);
        let codec_mask = hda_ctrl.detect_codecs();
        if codec_mask == 0 {
            return;
        }
        fstart_log::info!("intel-ich7: HDA codec mask = {:#x}", codec_mask);
        hda_ctrl.program_verb_tables(hda, codec_mask);
    }

    /// Platform lockdown (from coreboot `lpc_final`).
    ///
    /// Locks SPI, BIOS interface, global SMI, and TCO.
    pub fn lockdown(&self) {
        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);

        // Lock SPIBAR.
        let spi = rcba.read16(0x3800);
        rcba.write16(0x3800, spi | (1 << 15));

        // BIOS Interface Lockdown.
        rcba.write32(0x3410, rcba.read32(0x3410) | 1);

        // Global SMI Lock.
        ecam::or16(0, ich7::LPC_DEV, ich7::LPC_FUNC, 0xA0, 1 << 4);

        // TCO Lock.
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: PMBASE is valid.
            unsafe {
                let tco_addr = DEFAULT_PMBASE as u16 + 0x60 + 0x08;
                let tco1_cnt = fstart_pio::inw(tco_addr);
                fstart_pio::outw(tco_addr, tco1_cnt | (1 << 12));
            }
        }

        fstart_log::info!("intel-ich7: platform lockdown complete");
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — LPC bridge
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;
    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;

    use super::*;

    /// ICH7 PCI device IDs (LPC bridge variants).
    const LPC_PIDS: &[u16] = &[0x27B0, 0x27B8, 0x27B9, 0x27BC, 0x27BD];

    impl AcpiDevice for IntelIch7 {
        type Config = IntelIch7Config;

        /// Produce LPC bridge DSDT device node ("LPCB").
        ///
        /// Contains: `_HID` PNP0A05 (generic container), `_ADR` 0x001F0000,
        /// and nested ISA resource descriptors for legacy I/O ranges.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let name = config.acpi_name.as_deref().unwrap_or("LPCB");
            let _adr: u32 = 0x001F_0000; // B0:D31:F0

            fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_ADR", #{_adr});
                    Name("_HID", EisaId("PNP0A05"));
                }
            }
        }

        /// Produce standalone FADT fields as extra table bytes.
        ///
        /// The PM block addresses are carried in the x86 platform config
        /// (`X86PlatformAcpi`), so the LPC bridge does not produce any
        /// standalone tables. PIRQ routing is handled by the board RON
        /// `isos` (Interrupt Source Overrides) in the MADT.
        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }
}
