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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IntelIch7Config {
    /// Root Complex Base Address register value.
    pub rcba: u64,
    /// PIRQ routing (one byte per PIRQ A..H).
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits.
    pub gpe0_en: u32,
    /// LPC I/O decode register values (GEN1..GEN4).
    pub lpc_decode: [u32; 4],
    /// Enable HD Audio function.
    #[serde(default)]
    pub hd_audio: bool,
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
        if !self.config.hd_audio {
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
            config: *config,
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
