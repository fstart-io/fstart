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
// ---------------------------------------------------------------------------
// PM register offsets from PMBASE (0x500 default)
// ---------------------------------------------------------------------------

const PM1_STS: u16 = 0x00;
const PM1_EN: u16 = 0x02;
const PM1_CNT: u16 = 0x04;
const GPE0_STS: u16 = 0x28;
const GPE0_EN: u16 = 0x2C;
const SMI_EN: u16 = 0x30;
const SMI_STS: u16 = 0x34;
const ALT_GP_SMI_EN: u16 = 0x38;
const ALT_GP_SMI_STS: u16 = 0x3A;

// TCO register offsets from PMBASE + 0x60
const TCO_BASE_OFFSET: u16 = 0x60;
const TCO1_STS: u16 = 0x04;
const TCO2_STS: u16 = 0x06;
const TCO1_CNT: u16 = 0x08;

// SMI_EN bits
const GBL_SMI_EN: u32 = 1 << 0;
const EOS: u32 = 1 << 1;
const BIOS_EN: u32 = 1 << 2;
const SLP_SMI_EN: u32 = 1 << 4;
const APMC_EN: u32 = 1 << 5;
const TCO_EN: u32 = 1 << 13;
const PERIODIC_EN: u32 = 1 << 14;

// PM1_EN bits
const PWRBTN_EN: u16 = 1 << 8;
const GBL_EN: u16 = 1 << 5;

// GEN_PMCON_1 bits
const GEN_PMCON_1: u16 = 0xA0;
const SMI_LOCK: u16 = 1 << 4;

// GEN_PMCON_LOCK register
const GEN_PMCON_LOCK: u16 = 0xA6;
const ACPI_BASE_LOCK: u8 = 1 << 1;
const SLP_STR_POL_LOCK: u8 = 1 << 2;

// ETR3 register (LPC dev 0x1F func 0)
const ETR3: u16 = 0xAC;
const ETR3_CF9GR: u32 = 1 << 20;
const ETR3_CF9LOCK: u32 = 1 << 31;
const ETR3_CWORWRE: u32 = 1 << 18;

// IDE timing registers
const IDE_TIM_PRI: u16 = 0x40;
const IDE_TIM_SEC: u16 = 0x42;
const IDE_CONFIG: u16 = 0x54;

/// PM1_CNT register (offset from PMBASE).
const PM1_CNT_OFFSET: u16 = 0x04;
/// SLP_TYP mask in PM1_CNT [12:10].
const SLP_TYP_MASK: u32 = 0x1C00;
/// S3 (STR) SLP_TYP value.
const SLP_TYP_S3: u32 = 0x1400;

// GPIO register offsets now live in the shared fstart-gpio-ich crate.

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

// GPIO pad types are defined in the shared fstart-gpio-ich crate.
pub use fstart_gpio_ich::{GpioConfig, GpioPin, IchGpio};

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
    /// GPIO pad configuration (sets 1/2/3, all 76 pins).
    #[serde(default)]
    pub gpio: GpioConfig,
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
    /// Delegates to the shared `fstart-gpio-ich` crate which handles
    /// all three sets (76 pins) with the correct write ordering to
    /// prevent glitches on ICH7/ICH9M.
    fn setup_gpios(&self) {
        let gpio = IchGpio::new(DEFAULT_GPIOBASE as u16);
        gpio.setup(&self.config.gpio);
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

    /// Enhanced platform lockdown (from coreboot `finalize.c`).
    ///
    /// Performs additional lockdown beyond [`lockdown`](Self::lockdown):
    /// TC Lockdown, Function Disable SUS Well lock, ETR3 CF9 lock,
    /// GEN_PMCON lock bits, R/WO register lock.
    pub fn finalize(&self) {
        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // Run basic lockdown first.
        self.lockdown();

        // TCLOCKDN: TC Lockdown.
        rcba.write32(0x0050, rcba.read32(0x0050) | (1u32 << 31));

        // Function Disable SUS Well Lockdown.
        rcba.write8(0x3420, rcba.read8(0x3420) | (1 << 7));

        // GEN_PMCON_LOCK: ACPI base lock + SLP_STR policy lock.
        ecam::or8(0, d, f, GEN_PMCON_LOCK, ACPI_BASE_LOCK | SLP_STR_POL_LOCK);

        // ETR3: clear CF9 global reset, set CF9 lock.
        ecam::modify32(0, d, f, ETR3, !ETR3_CF9GR, ETR3_CF9LOCK);

        // R/WO register lock (read-then-write-back).
        rcba.write32(0x21A4, rcba.read32(0x21A4));
        // HDA R/WO register.
        let hda_rwo = ecam::read32(0, 0x1B, 0, 0x74);
        ecam::write32(0, 0x1B, 0, 0x74, hda_rwo);

        fstart_log::info!("intel-ich7: finalize complete");
    }

    // -----------------------------------------------------------------------
    // PM register helpers (PIO-based, offset from PMBASE)
    // -----------------------------------------------------------------------

    /// Read a 32-bit PM register at `offset` from PMBASE.
    #[cfg(target_arch = "x86_64")]
    fn pm_read32(&self, offset: u16) -> u32 {
        // SAFETY: PMBASE is a valid I/O base programmed during early_init.
        unsafe { fstart_pio::inl(DEFAULT_PMBASE as u16 + offset) }
    }

    /// Write a 32-bit PM register at `offset` from PMBASE.
    #[cfg(target_arch = "x86_64")]
    fn pm_write32(&self, offset: u16, val: u32) {
        // SAFETY: PMBASE is a valid I/O base.
        unsafe { fstart_pio::outl(DEFAULT_PMBASE as u16 + offset, val) }
    }

    /// Read a 16-bit PM register.
    #[cfg(target_arch = "x86_64")]
    fn pm_read16(&self, offset: u16) -> u16 {
        unsafe { fstart_pio::inw(DEFAULT_PMBASE as u16 + offset) }
    }

    /// Write a 16-bit PM register.
    #[cfg(target_arch = "x86_64")]
    fn pm_write16(&self, offset: u16, val: u16) {
        unsafe { fstart_pio::outw(DEFAULT_PMBASE as u16 + offset, val) }
    }

    // -----------------------------------------------------------------------
    // PM/SMI/GPE/TCO status management (from pmutil.c)
    // -----------------------------------------------------------------------

    /// Read and clear PM1_STS (write-1-to-clear).
    #[cfg(target_arch = "x86_64")]
    pub fn reset_pm1_status(&self) -> u16 {
        let sts = self.pm_read16(PM1_STS);
        self.pm_write16(PM1_STS, sts);
        sts
    }

    /// Read and clear SMI_STS (write-1-to-clear).
    #[cfg(target_arch = "x86_64")]
    pub fn reset_smi_status(&self) -> u32 {
        let sts = self.pm_read32(SMI_STS);
        self.pm_write32(SMI_STS, sts);
        sts
    }

    /// Read and clear GPE0_STS (write-1-to-clear).
    #[cfg(target_arch = "x86_64")]
    pub fn reset_gpe0_status(&self) -> u32 {
        let sts = self.pm_read32(GPE0_STS);
        self.pm_write32(GPE0_STS, sts);
        sts
    }

    /// Read and clear TCO status registers (write-1-to-clear).
    ///
    /// Returns combined TCO1_STS | (TCO2_STS << 16).
    #[cfg(target_arch = "x86_64")]
    pub fn reset_tco_status(&self) -> u32 {
        let tco = DEFAULT_PMBASE as u16 + TCO_BASE_OFFSET;
        unsafe {
            let tco1 = fstart_pio::inw(tco + TCO1_STS);
            let tco2 = fstart_pio::inw(tco + TCO2_STS);
            // Clear BOOT_STS after SECOND_TO_STS per spec.
            // Clear all status bits EXCEPT BOOT_STS (bit 2) first.
            fstart_pio::outw(tco + TCO1_STS, tco1 & !4u16);
            // Then clear BOOT_STS separately (must happen after SECOND_TO).
            if tco1 & 4 != 0 {
                fstart_pio::outw(tco + TCO1_STS, 4);
            }
            fstart_pio::outw(tco + TCO2_STS, tco2);
            (tco1 as u32) | ((tco2 as u32) << 16)
        }
    }

    /// Read and clear ALT_GP_SMI_STS.
    #[cfg(target_arch = "x86_64")]
    pub fn reset_alt_gp_smi_status(&self) -> u16 {
        let sts = self.pm_read16(ALT_GP_SMI_STS);
        self.pm_write16(ALT_GP_SMI_STS, sts);
        sts
    }

    /// Clear all PM/SMI/GPE/TCO status registers.
    ///
    /// Called before enabling SMIs to start from a clean state.
    /// Ported from coreboot `smm_southbridge_clear_state()`.
    #[cfg(target_arch = "x86_64")]
    pub fn clear_pm_status(&self) {
        self.reset_smi_status();
        self.reset_pm1_status();
        self.reset_tco_status();
        self.reset_gpe0_status();
    }

    // -----------------------------------------------------------------------
    // SMI enable (from smi.c)
    // -----------------------------------------------------------------------

    /// Enable global SMI generation.
    ///
    /// Programs SMI_EN for TCO, APMC (APM port 0xB2), and SLP_SMI
    /// events.  Sets GBL_SMI_EN + EOS to activate the SMI logic.
    ///
    /// Called after SMM handler installation and relocation.
    /// Ported from coreboot `global_smi_enable()`.
    #[cfg(target_arch = "x86_64")]
    pub fn global_smi_enable(&self) {
        let smi_en = self.pm_read32(SMI_EN);
        if smi_en & APMC_EN != 0 {
            fstart_log::info!("intel-ich7: SMI already enabled");
            return;
        }

        // Clear all status registers first.
        self.clear_pm_status();

        // Enable PM1 events: power button + global.
        self.pm_write16(PM1_EN, PWRBTN_EN | GBL_EN);

        // Enable SMI sources: TCO, APMC, SLP_SMI, GBL_SMI + EOS.
        let smi = TCO_EN | APMC_EN | SLP_SMI_EN | GBL_SMI_EN | EOS;
        self.pm_write32(SMI_EN, smi);

        fstart_log::info!("intel-ich7: global SMI enabled");
    }

    // -----------------------------------------------------------------------
    // System reset (from reset.c + me.c)
    // -----------------------------------------------------------------------

    /// Trigger a system reset via CF9 port.
    ///
    /// Writes 0x02 (soft reset) or 0x06 (hard reset) to port 0xCF9.
    pub fn system_reset(&self, hard: bool) -> ! {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            // Ensure CF9GR is cleared (no global reset).
            let etr3 = ecam::read32(0, ich7::LPC_DEV, ich7::LPC_FUNC, ETR3);
            ecam::write32(
                0,
                ich7::LPC_DEV,
                ich7::LPC_FUNC,
                ETR3,
                (etr3 & !ETR3_CF9GR) & !ETR3_CWORWRE,
            );

            let val: u8 = if hard { 0x06 } else { 0x02 };
            fstart_pio::outb(0xCF9, 0x00); // Clear first
            fstart_pio::outb(0xCF9, val);
        }
        loop {
            core::hint::spin_loop();
        }
    }

    /// Configure CF9 for global reset (ME reset).
    ///
    /// Sets ETR3.CF9GR so the next CF9 write triggers a full platform
    /// reset including ME.  Ported from coreboot `set_global_reset()`.
    pub fn set_global_reset(&self, enable: bool) {
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;
        let mut etr3 = ecam::read32(0, d, f, ETR3);
        etr3 &= !ETR3_CWORWRE;
        if enable {
            etr3 |= ETR3_CF9GR;
        } else {
            etr3 &= !ETR3_CF9GR;
        }
        ecam::write32(0, d, f, ETR3, etr3);
    }

    // -----------------------------------------------------------------------
    // RTC / CMOS init (from rtc.c)
    // -----------------------------------------------------------------------

    /// Check if the RTC battery died (GEN_PMCON_3 bit 2).
    pub fn rtc_failure(&self) -> bool {
        ecam::read8(0, ich7::LPC_DEV, ich7::LPC_FUNC, GEN_PMCON_3) & RTC_BATTERY_DEAD != 0
    }

    /// Initialize the RTC / CMOS.
    ///
    /// If the RTC battery died, clears the status bit and initialises
    /// CMOS to defaults.  Otherwise just validates the checksum.
    ///
    /// Ported from coreboot `sb_rtc_init()`. The actual CMOS init
    /// (mc146818 register programming) is a sequence of port 0x70/0x71
    /// writes that sets up the RTC oscillator and clears CMOS RAM.
    pub fn rtc_init(&self) {
        let failed = self.rtc_failure();
        if failed {
            // Clear the RTC battery dead bit.
            ecam::and8(
                0,
                ich7::LPC_DEV,
                ich7::LPC_FUNC,
                GEN_PMCON_3,
                !RTC_BATTERY_DEAD,
            );
            fstart_log::info!("intel-ich7: RTC battery dead — reinitializing CMOS");
        }

        #[cfg(target_arch = "x86_64")]
        unsafe {
            // Standard CMOS/RTC initialization:
            // Register A: divider = 32.768 KHz, rate = 1024 Hz.
            fstart_pio::outb(0x70, 0x0A);
            fstart_pio::outb(0x71, 0x26);
            // Register B: 24hr mode, BCD, no alarms, update enabled.
            fstart_pio::outb(0x70, 0x0B);
            let reg_b = fstart_pio::inb(0x71);
            fstart_pio::outb(0x70, 0x0B);
            fstart_pio::outb(0x71, (reg_b | 0x02) & !0x40); // 24hr, update enabled
                                                            // Register C: clear interrupt flags (read-to-clear).
            fstart_pio::outb(0x70, 0x0C);
            let _ = fstart_pio::inb(0x71);
            // Register D: read-only, but reading clears VRT.
            fstart_pio::outb(0x70, 0x0D);
            let _ = fstart_pio::inb(0x71);
        }
    }

    // -----------------------------------------------------------------------
    // PCI-PCI bridge init (dev 0x1E, from pci.c)
    // -----------------------------------------------------------------------

    /// Initialize the PCI-PCI bridge at dev 0x1E func 0.
    ///
    /// Enables bus master, sets master latency timer, disables parity
    /// and SERR on the bridge.  Ported from coreboot `pci.c::pci_init()`.
    pub fn pci_bridge_init(&self) {
        let d: u8 = 0x1E;
        let f: u8 = 0;

        let vid = ecam::read16(0, d, f, 0x00);
        if vid == 0xFFFF {
            return;
        }

        // Enable bus master.
        ecam::or16(0, d, f, 0x04, 0x04); // BusMaster only

        // No interrupt.
        ecam::write8(0, d, f, 0x3C, 0xFF);

        // Disable parity + SERR on bridge control.
        ecam::and16(0, d, f, 0x3E, !(0x01 | 0x02));

        // Master Latency Timer = 0x04 << 3 (keep low bits).
        ecam::and8_or8(0, d, f, SMLT, 0x07, 0x04 << 3);

        fstart_log::info!("intel-ich7: PCI bridge (1E.0) init");
    }

    // -----------------------------------------------------------------------
    // IDE / PATA init (dev 0x1F func 1, from ide.c)
    // -----------------------------------------------------------------------

    /// Initialize the IDE (PATA) controller at dev 0x1F func 1.
    ///
    /// Configures primary and/or secondary channels with decode enable,
    /// timing, and I/O configuration.  Ported from coreboot `ide.c`.
    pub fn ide_init(&self, enable_primary: bool, enable_secondary: bool) {
        let d: u8 = 0x1F;
        let f: u8 = 1;

        let vid = ecam::read16(0, d, f, 0x00);
        if vid == 0xFFFF {
            return;
        }

        // Enable IO + BusMaster.
        ecam::or16(0, d, f, 0x04, 0x05);

        // Native capable, not enabled.
        ecam::write8(0, d, f, 0x09, 0x8A);

        // IDE timing bits.
        const IDE_DECODE_ENABLE: u16 = 1 << 15;
        const IDE_SITRE: u16 = 1 << 14;
        const IDE_ISP_3: u16 = 0x3000; // ISP = 3 clocks
        const IDE_RCT_1: u16 = 0x0300; // RCT = 1 clock
        const IDE_IE0: u16 = 1 << 1;
        const IDE_TIME0: u16 = 1 << 0;

        // Primary channel.
        let mut tim = ecam::read16(0, d, f, IDE_TIM_PRI);
        tim &= !IDE_DECODE_ENABLE;
        tim |= IDE_SITRE;
        if enable_primary {
            tim |= IDE_DECODE_ENABLE | IDE_ISP_3 | IDE_RCT_1 | IDE_IE0 | IDE_TIME0;
        }
        ecam::write16(0, d, f, IDE_TIM_PRI, tim);

        // Secondary channel.
        tim = ecam::read16(0, d, f, IDE_TIM_SEC);
        tim &= !IDE_DECODE_ENABLE;
        tim |= IDE_SITRE;
        if enable_secondary {
            tim |= IDE_DECODE_ENABLE | IDE_ISP_3 | IDE_RCT_1 | IDE_IE0 | IDE_TIME0;
        }
        ecam::write16(0, d, f, IDE_TIM_SEC, tim);

        // IDE I/O configuration.
        let mut cfg = 0u32;
        if enable_primary {
            cfg |= 0x0003_0003; // SIG_MODE_PRI_NORMAL + FAST_PCBx + PCBx
        }
        if enable_secondary {
            cfg |= 0x0030_0030; // SIG_MODE_SEC_NORMAL + FAST_SCBx + SCBx
        }
        ecam::write32(0, d, f, IDE_CONFIG, cfg);

        // Interrupt line = 0xFF (unused).
        ecam::write8(0, d, f, 0x3C, 0xFF);

        fstart_log::info!(
            "intel-ich7: IDE init (pri={} sec={})",
            enable_primary,
            enable_secondary
        );
    }

    // -----------------------------------------------------------------------
    // Watchdog control (standalone, beyond early_init TCO halt)
    // -----------------------------------------------------------------------

    /// Disable the ICH watchdog timer.
    ///
    /// Halts the TCO timer and clears timeout status.  More thorough
    /// than the early_init TCO halt — also disables PCI interrupts.
    /// Ported from coreboot `watchdog_off()`.
    pub fn watchdog_off(&self) {
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // Disable PCI interrupts.
        ecam::or16(0, d, f, 0x04, 1 << 10); // PCI_COMMAND_INT_DISABLE

        #[cfg(target_arch = "x86_64")]
        unsafe {
            let tco = DEFAULT_PMBASE as u16 + TCO_BASE_OFFSET;
            // Halt TCO timer.
            let cnt = fstart_pio::inw(tco + TCO1_CNT);
            fstart_pio::outw(tco + TCO1_CNT, cnt | (1 << 11));
            // Clear timeout status.
            fstart_pio::outw(tco + TCO1_STS, 1 << 3);
            fstart_pio::outw(tco + TCO2_STS, 1 << 1);
        }
    }

    // -----------------------------------------------------------------------
    // Power off (S5 entry)
    // -----------------------------------------------------------------------

    /// Enter S5 (soft-off) state.
    ///
    /// Writes SLP_TYP=S5 + SLP_EN to PM1_CNT.  Does not return.
    pub fn poweroff(&self) -> ! {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let pm = DEFAULT_PMBASE as u16;
            let mut pm1 = fstart_pio::inl(pm + PM1_CNT);
            pm1 |= 0x3C00; // SLP_TYP = S5 (bits [12:10] = 0xF)
            pm1 |= 1 << 13; // SLP_EN
            fstart_pio::outl(pm + PM1_CNT, pm1);
        }
        loop {
            core::hint::spin_loop();
        }
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
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    /// ICH7 PCI device IDs (LPC bridge variants).
    const LPC_PIDS: &[u16] = &[0x27B0, 0x27B8, 0x27B9, 0x27BC, 0x27BD];

    impl AcpiDevice for IntelIch7 {
        type Config = IntelIch7Config;

        /// Produce ICH7 DSDT content for the LPC bridge and all its PCI
        /// function siblings.
        ///
        /// The output is a flat sequence of AML objects.  The caller is
        /// expected to embed them inside the appropriate PCI0 scope.
        /// Sleep-state objects (`_S0_`, `_S5_`) are appended at the end
        /// for placement at root scope by the caller.
        ///
        /// # LPCB children (ISA legacy devices)
        ///
        /// `DMAC` — 8237 DMA controller (PNP0200)
        /// `FWH_` — Firmware Hub (INT0800)
        /// `HPET` — High Precision Event Timer (PNP0103)
        /// `PIC_` — 8259 interrupt controller (PNP0000)
        /// `MATH` — FPU / x87 (PNP0C04)
        /// `LDRC` — LPC resource consumption (PNP0C02)
        /// `RTC_` — Real Time Clock (PNP0B00)
        /// `TIMR` — 8254 timer (PNP0100)
        ///
        /// # PCI function siblings (at PCI0 scope level)
        ///
        /// `HDEF` 0:1B.0, `USB1`–`USB4` 0:1D.0-3, `EHC1` 0:1D.7,
        /// `RP01`–`RP06` 0:1C.0-5, `PCIB` 0:1E.0,
        /// `SATA` 0:1F.2, `PATA` 0:1F.1, `SBUS` 0:1F.3.
        ///
        /// Ported from coreboot `src/southbridge/intel/i82801gx/acpi/`.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let name = config.acpi_name.as_deref().unwrap_or("LPCB");
            let _adr: u32 = 0x001F_0000; // B0:D31:F0

            // ---------------------------------------------------------------
            // 1. LPCB device with all ISA legacy children.
            //
            // IRQNoFlags() is not a supported macro keyword; we substitute
            // the extended Interrupt() descriptor (opcode 0x89) which Linux
            // handles equivalently for ISA legacy IRQs.
            //
            // DMA() channel descriptor is also unsupported by the macro and
            // is omitted — Linux does not require it for boot enumeration.
            // ---------------------------------------------------------------
            let mut aml = acpi_dsl! {
                Device(#{name}) {
                    Name("_ADR", #{_adr});
                    Name("_HID", EisaId("PNP0A05"));

                    // LPC PCI config OpRegion for PIRQ routing registers.
                    // PRTA-PRTH at offsets 0x60-0x63 and 0x68-0x6B.
                    // Read by PIRQ link devices (LNKA-LNKH) and by the
                    // OS to discover current IRQ assignments.
                    // Coreboot: lpc.asl OperationRegion(LPC0)
                    OperationRegion("LPC0", PciConfig, 0x00u32, 0x100u32);
                    Field("LPC0", AnyAcc, NoLock, Preserve) {
                        Offset(0x60),
                        PRTA, 8,
                        PRTB, 8,
                        PRTC, 8,
                        PRTD, 8,
                        Offset(0x68),
                        PRTE, 8,
                        PRTF, 8,
                        PRTG, 8,
                        PRTH, 8,
                    }

                    // DMAC — 8237 DMA Controller (PNP0200)
                    // Coreboot: lpc.asl Device(DMAC)
                    Device("DMAC") {
                        Name("_HID", EisaId("PNP0200"));
                        Name("_CRS", ResourceTemplate {
                            IO(0x0000u16, 0x0000u16, 0x01u8, 0x20u8);
                            IO(0x0081u16, 0x0081u16, 0x01u8, 0x11u8);
                            IO(0x0093u16, 0x0093u16, 0x01u8, 0x0Du8);
                            IO(0x00C0u16, 0x00C0u16, 0x01u8, 0x20u8);
                        });
                    }

                    // FWH_ — Firmware Hub (INT0800)
                    // Coreboot: lpc.asl Device(FWH)
                    Device("FWH_") {
                        Name("_HID", EisaId("INT0800"));
                        Name("_CRS", ResourceTemplate {
                            Memory32Fixed(ReadOnly, 0xFF000000u32, 0x01000000u32);
                        });
                    }

                    // HPET — High Precision Event Timer (PNP0103)
                    // Fixed at 0xFED00000, 0x400 bytes.
                    // Coreboot: lpc.asl Device(HPET)
                    Device("HPET") {
                        Name("_HID", EisaId("PNP0103"));
                        Name("_CRS", ResourceTemplate {
                            Memory32Fixed(ReadOnly, 0xFED00000u32, 0x400u32);
                        });
                    }

                    // PIC_ — 8259 Programmable Interrupt Controller (PNP0000)
                    // Coreboot: lpc.asl Device(PIC)
                    Device("PIC_") {
                        Name("_HID", EisaId("PNP0000"));
                        Name("_CRS", ResourceTemplate {
                            IO(0x0020u16, 0x0020u16, 0x01u8, 0x02u8);
                            IO(0x0024u16, 0x0024u16, 0x01u8, 0x02u8);
                            IO(0x0028u16, 0x0028u16, 0x01u8, 0x02u8);
                            IO(0x002Cu16, 0x002Cu16, 0x01u8, 0x02u8);
                            IO(0x0030u16, 0x0030u16, 0x01u8, 0x02u8);
                            IO(0x0034u16, 0x0034u16, 0x01u8, 0x02u8);
                            IO(0x0038u16, 0x0038u16, 0x01u8, 0x02u8);
                            IO(0x003Cu16, 0x003Cu16, 0x01u8, 0x02u8);
                            IO(0x00A0u16, 0x00A0u16, 0x01u8, 0x02u8);
                            IO(0x00A4u16, 0x00A4u16, 0x01u8, 0x02u8);
                            IO(0x00A8u16, 0x00A8u16, 0x01u8, 0x02u8);
                            IO(0x00ACu16, 0x00ACu16, 0x01u8, 0x02u8);
                            IO(0x00B0u16, 0x00B0u16, 0x01u8, 0x02u8);
                            IO(0x00B4u16, 0x00B4u16, 0x01u8, 0x02u8);
                            IO(0x00B8u16, 0x00B8u16, 0x01u8, 0x02u8);
                            IO(0x00BCu16, 0x00BCu16, 0x01u8, 0x02u8);
                            IO(0x04D0u16, 0x04D0u16, 0x01u8, 0x02u8);
                            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 2u32);
                        });
                    }

                    // MATH — FPU / x87 co-processor (PNP0C04)
                    // Coreboot: lpc.asl Device(MATH)
                    Device("MATH") {
                        Name("_HID", EisaId("PNP0C04"));
                        Name("_CRS", ResourceTemplate {
                            IO(0x00F0u16, 0x00F0u16, 0x01u8, 0x01u8);
                            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 13u32);
                        });
                    }

                    // LDRC — LPC device resource consumption (PNP0C02)
                    // Covers SuperIO ports, NMI, POST, ACPI I/O, PMBASE, GPIOBASE.
                    // Coreboot: lpc.asl Device(LDRC)
                    Device("LDRC") {
                        Name("_HID", EisaId("PNP0C02"));
                        Name("_UID", 2u32);
                        Name("_CRS", ResourceTemplate {
                            IO(0x002Eu16, 0x002Eu16, 0x01u8, 0x02u8);
                            IO(0x004Eu16, 0x004Eu16, 0x01u8, 0x02u8);
                            IO(0x0061u16, 0x0061u16, 0x01u8, 0x01u8);
                            IO(0x0063u16, 0x0063u16, 0x01u8, 0x01u8);
                            IO(0x0065u16, 0x0065u16, 0x01u8, 0x01u8);
                            IO(0x0067u16, 0x0067u16, 0x01u8, 0x01u8);
                            IO(0x0080u16, 0x0080u16, 0x01u8, 0x01u8);
                            IO(0x0092u16, 0x0092u16, 0x01u8, 0x01u8);
                            IO(0x00B2u16, 0x00B2u16, 0x01u8, 0x02u8);
                            IO(0x0800u16, 0x0800u16, 0x01u8, 0x10u8);
                            IO(0x0500u16, 0x0500u16, 0x01u8, 0x80u8);
                            IO(0x0480u16, 0x0480u16, 0x01u8, 0x40u8);
                        });
                    }

                    // RTC_ — Real Time Clock (PNP0B00)
                    // Coreboot: lpc.asl Device(RTC)
                    Device("RTC_") {
                        Name("_HID", EisaId("PNP0B00"));
                        Name("_CRS", ResourceTemplate {
                            IO(0x0070u16, 0x0070u16, 0x01u8, 0x08u8);
                        });
                    }

                    // TIMR — 8254 Programmable Interval Timer (PNP0100)
                    // Coreboot: lpc.asl Device(TIMR)
                    Device("TIMR") {
                        Name("_HID", EisaId("PNP0100"));
                        Name("_CRS", ResourceTemplate {
                            IO(0x0040u16, 0x0040u16, 0x01u8, 0x04u8);
                            IO(0x0050u16, 0x0050u16, 0x10u8, 0x04u8);
                            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 0u32);
                        });
                    }

                    // -------------------------------------------------------
                    // PIRQ link devices LNKA-LNKH (PNP0C0F)
                    //
                    // These represent the ICH7’s 8 PCI interrupt routing
                    // links.  Full _CRS/_SRS methods (which read/write
                    // the PRTA-PRTH fields above) require CreateWordField
                    // and FindSetRightBit, which are not yet supported
                    // by the acpi_dsl macro.  The stubs below give each
                    // link a _UID and _STA (active).  Linux uses the
                    // APIC-mode _PRT entries on PCIe root ports (with
                    // direct GSI numbers) when IOAPIC is available, so
                    // these stubs are sufficient for APIC-mode boot.
                    //
                    // Coreboot: irqlinks.asl
                    // -------------------------------------------------------
                    Device("LNKA") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 1u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKB") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 2u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKC") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 3u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKD") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 4u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKE") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 5u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKF") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 6u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKG") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 7u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                    Device("LNKH") {
                        Name("_HID", EisaId("PNP0C0F"));
                        Name("_UID", 8u32);
                        Method("_STA", 0, NotSerialized) { Return(0x0Bu32); }
                    }
                }
            };

            // ---------------------------------------------------------------
            // 2. Per-function PCI device nodes — siblings to LPCB at PCI0
            //    scope level.  Includes _PRW (Power Resources for Wake)
            //    for USB and HDA devices.
            //
            //    Ported from coreboot:
            //      audio_ich.asl, usb.asl, pcie.asl, pci.asl,
            //      sata.asl, pata.asl, smbus.asl
            // ---------------------------------------------------------------

            // HDEF — HD Audio controller  0:1B.0
            // _PRW: GPE bit 5, can wake from S4.
            aml.extend_from_slice(&acpi_dsl! {
                Device("HDEF") {
                    Name("_ADR", 0x001B0000u32);
                    Name("_PRW", Package(5u32, 4u32));
                }
            });

            // USB UHCI controllers  0:1D.0–3
            // _PRW: GPE bit 3, can wake from S4.
            // _S3D/_S4D: highest D-state in S3/S4 (D2 — USB stays
            //   partially powered for wake-on-USB).
            // Coreboot: usb.asl USB1–USB4
            aml.extend_from_slice(&acpi_dsl! {
                Device("USB1") {
                    Name("_ADR", 0x001D0000u32);
                    Name("_PRW", Package(3u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }
                }
            });
            aml.extend_from_slice(&acpi_dsl! {
                Device("USB2") {
                    Name("_ADR", 0x001D0001u32);
                    Name("_PRW", Package(3u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }
                }
            });
            aml.extend_from_slice(&acpi_dsl! {
                Device("USB3") {
                    Name("_ADR", 0x001D0002u32);
                    Name("_PRW", Package(3u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }
                }
            });
            aml.extend_from_slice(&acpi_dsl! {
                Device("USB4") {
                    Name("_ADR", 0x001D0003u32);
                    Name("_PRW", Package(3u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }
                }
            });

            // EHC1 — EHCI USB 2.0 controller  0:1D.7
            // Includes root hub (HUB7) with 6 port child devices.
            // _PRW: GPE bit 13, can wake from S4.
            // Coreboot: usb.asl EHC1 + HUB7
            aml.extend_from_slice(&acpi_dsl! {
                Device("EHC1") {
                    Name("_ADR", 0x001D0007u32);
                    Name("_PRW", Package(13u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }

                    Device("HUB7") {
                        Name("_ADR", 0x00000000u32);
                        Device("PRT1") { Name("_ADR", 1u32); }
                        Device("PRT2") { Name("_ADR", 2u32); }
                        Device("PRT3") { Name("_ADR", 3u32); }
                        Device("PRT4") { Name("_ADR", 4u32); }
                        Device("PRT5") { Name("_ADR", 5u32); }
                        Device("PRT6") { Name("_ADR", 6u32); }
                    }
                }
            });

            // ---------------------------------------------------------------
            // PCIe root ports  0:1C.0–5 with APIC-mode _PRT routing.
            //
            // Each root port's _PRT maps INTA-INTD to IOAPIC GSIs 16-19
            // with a rotation based on port number (matching coreboot's
            // pcie.asl IRQM method).  The rotation is:
            //   Port 1,5: A→16 B→17 C→18 D→19
            //   Port 2,6: A→17 B→18 C→19 D→16
            //   Port 3:   A→18 B→19 C→16 D→17
            //   Port 4:   A→19 B→16 C→17 D→18
            //
            // We emit APIC-mode routing only (GSI values, no link
            // devices) since the kernel uses IOAPIC when available.
            // ---------------------------------------------------------------

            // Helper: emit a PCIe root port with APIC-mode _PRT.
            // `port_num` is 1-based (matches coreboot convention).
            let emit_rp = |name: &str, adr: u32, port_num: u32| -> Vec<u8> {
                let base = ((port_num - 1) % 4) as u32;
                let a = 16 + base;
                let b = 16 + (base + 1) % 4;
                let c = 16 + (base + 2) % 4;
                let d = 16 + (base + 3) % 4;

                // Each root port has:
                //  - RPCS: PCI config OpRegion for hotplug status
                //    (RPPN = root port number, PDC = presence detect,
                //     HPCS = hot-plug capable slot)
                //  - _PRT: APIC-mode interrupt routing table
                //
                // Coreboot: pcie.asl + pcie_port.asl
                acpi_dsl! {
                    Device(#{name}) {
                        Name("_ADR", #{adr});

                        OperationRegion("RPCS", PciConfig, 0x00u32, 0xFFu32);
                        Field("RPCS", AnyAcc, NoLock, Preserve) {
                            Offset(0x4C),
                            , 24,
                            RPPN, 8,
                            Offset(0x5A),
                            , 3,
                            PDC_, 1,
                            Offset(0xDF),
                            , 6,
                            HPCS, 1,
                        }

                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, #{a}),
                            Package(0x0000FFFFu32, 1u32, 0u32, #{b}),
                            Package(0x0000FFFFu32, 2u32, 0u32, #{c}),
                            Package(0x0000FFFFu32, 3u32, 0u32, #{d})
                        ));
                    }
                }
            };

            aml.extend_from_slice(&emit_rp("RP01", 0x001C0000, 1));
            aml.extend_from_slice(&emit_rp("RP02", 0x001C0001, 2));
            aml.extend_from_slice(&emit_rp("RP03", 0x001C0002, 3));
            aml.extend_from_slice(&emit_rp("RP04", 0x001C0003, 4));
            aml.extend_from_slice(&emit_rp("RP05", 0x001C0004, 5));
            aml.extend_from_slice(&emit_rp("RP06", 0x001C0005, 6));

            // PCIB — PCI-to-PCI bridge  0:1E.0
            // Includes child slot devices with _PRW for wake support
            // and _PRT interrupt routing (APIC mode: GSI 0x14-0x17).
            // Coreboot: pci.asl + mainboard ich7_pci_irqs.asl
            aml.extend_from_slice(&acpi_dsl! {
                Device("PCIB") {
                    Name("_ADR", 0x001E0000u32);

                    // PCI bridge _PRT (APIC mode).
                    // GSI mapping for devices behind the bridge.
                    Name("_PRT", Package(
                        Package(0x0000FFFFu32, 0u32, 0u32, 0x15u32),
                        Package(0x0000FFFFu32, 1u32, 0u32, 0x16u32),
                        Package(0x0000FFFFu32, 2u32, 0u32, 0x17u32),
                        Package(0x0000FFFFu32, 3u32, 0u32, 0x14u32),
                        Package(0x0001FFFFu32, 0u32, 0u32, 0x13u32)
                    ));
                }
            });

            // PEGP — PCI Express Graphics port  0:1.0
            // PCIe x16 slot for discrete GPU (Pineview).
            // Coreboot: peg.asl
            aml.extend_from_slice(&acpi_dsl! {
                Device("PEGP") {
                    Name("_ADR", 0x00010000u32);
                    Name("_PRT", Package(
                        Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                        Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                        Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                        Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
                    ));
                }
            });

            // GFX0 — Integrated Graphics Device  0:2.0
            // Stub power management methods for the Intel GMA.
            // Coreboot: drivers/intel/gma/acpi/gfx.asl
            aml.extend_from_slice(&acpi_dsl! {
                Device("GFX0") {
                    Name("_ADR", 0x00020000u32);
                    // Power state stubs.
                    Method("_PS0", 0, NotSerialized) { }
                    Method("_PS3", 0, NotSerialized) { }
                    // Highest D-state from which device can wake in S0.
                    Method("_S0W", 0, NotSerialized) { Return(3u32); }
                    // Highest D-state in S3.
                    Method("_S3D", 0, NotSerialized) { Return(3u32); }
                }
            });

            // SATA — SATA controller  0:1F.2
            aml.extend_from_slice(&acpi_dsl! {
                Device("SATA") {
                    Name("_ADR", 0x001F0002u32);
                }
            });

            // PATA — IDE / PATA controller  0:1F.1
            aml.extend_from_slice(&acpi_dsl! {
                Device("PATA") {
                    Name("_ADR", 0x001F0001u32);
                }
            });

            // SBUS — SMBus controller  0:1F.3
            aml.extend_from_slice(&acpi_dsl! {
                Device("SBUS") {
                    Name("_ADR", 0x001F0003u32);
                }
            });

            // ---------------------------------------------------------------
            // 3. Root-scope objects.
            // ---------------------------------------------------------------

            // _PIC method + PICM variable.
            //
            // The OS calls _PIC(1) during boot to signal APIC mode.
            // PICM is used by _PRT methods to select PIC vs APIC routing.
            // Since we only emit APIC-mode _PRT tables, PICM is
            // informational — but Linux expects _PIC to exist.
            // Coreboot: dsdt_top.asl
            aml.extend_from_slice(&acpi_dsl! {
                Name("PICM", 0u32);
                Method("_PIC", 1, NotSerialized) {
                    Store(#{fstart_acpi::aml::Arg(0)}, #{fstart_acpi::aml::Path::new("PICM")});
                }
            });

            // System sleep states.
            // S0 = working, S3 = suspend-to-RAM, S4 = hibernate, S5 = soft-off.
            // SLP_TYP values match ICH7 PM1_CNT encoding.
            // Coreboot: sleepstates.asl
            aml.extend_from_slice(&acpi_dsl! {
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
            });

            aml
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
