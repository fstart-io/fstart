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

use fstart_pineview_regs::{ich7, EcamPci, Rcba};
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
    fn ecam(&self) -> EcamPci {
        EcamPci::new(self.config.ecam_base as usize)
    }

    /// CIR (Chipset Initialization Registers) magic writes.
    ///
    /// Ported from coreboot `ich7_setup_cir()`.
    fn setup_cir(&self, rcba: &Rcba, _ecam: &EcamPci) {
        rcba.write32(0x0088, 0x0011_D000);
        rcba.write32(0x01F4, 0x8600_0040);
        rcba.write32(0x0214, 0x1003_0549);
        rcba.write32(0x0218, 0x0002_0504);
        rcba.write8(0x0220, 0xC5);
        // RCBA 0x3430: clear bits [1:0], set bit 0.
        let v = rcba.read32(0x3430);
        rcba.write32(0x3430, (v & !3) | 1);

        fstart_log::info!("intel-ich7: CIR configured");
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
        let ecam = self.ecam();
        let d = ich7::LPC_DEV;
        let f = ich7::LPC_FUNC;

        // ---- 1. Enable SMBus (must be first — raminit reads SPD) ----
        let smbus = I801SmBus::enable_on_ich7(&ecam, self.config.smbus_base);
        self.smbus = Some(smbus);

        // ---- 2. Setup BARs ----
        // RCBA
        ecam.write32(
            0,
            d,
            f,
            ich7::RCBA_REG,
            (self.config.rcba as u32 & 0xFFFF_C000) | 1,
        );
        // PMBASE + ACPI enable
        ecam.write32(0, d, f, PMBASE_REG, DEFAULT_PMBASE | 1);
        ecam.write8(0, d, f, ACPI_CNTL, ACPI_EN);
        // GPIOBASE + GPIO enable
        ecam.write32(0, d, f, GPIOBASE_REG, DEFAULT_GPIOBASE | 1);
        ecam.write8(0, d, f, GPIO_CNTL, GPIO_EN);

        // ---- 3. Serial IRQ configuration ----
        ecam.write8(0, d, f, SERIRQ_CNTL, 0xD0);

        // ---- 4. LPC I/O decode and enable ----
        ecam.write16(0, d, f, LPC_IO_DEC, 0x0010);
        // Enable: SuperIO (CNF1/CNF2), KBC, COMA, COMB, LPT, FDD, GAME
        ecam.write16(0, d, f, LPC_EN, LPC_EN_ALL);
        // Generic decode ranges (GEN1..GEN4) from board config.
        ecam.write32(0, d, f, GEN1_DEC, self.config.lpc_decode[0]);
        ecam.write32(0, d, f, GEN2_DEC, self.config.lpc_decode[1]);
        ecam.write32(0, d, f, GEN3_DEC, self.config.lpc_decode[2]);
        ecam.write32(0, d, f, GEN4_DEC, self.config.lpc_decode[3]);

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
        ecam.write32(0, d, f, 0x60, pirq_low);
        ecam.write32(0, d, f, 0x68, pirq_high);

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
        ecam.write8(0, 0x1e, 0, SMLT, 0x20);

        // ---- 8. Reset RTC power status ----
        ecam.and8(0, d, f, GEN_PMCON_3, !RTC_BATTERY_DEAD);

        // ---- 9. USB pre-config ----
        ecam.or8(0, d, f, 0xAD, 3);
        ecam.or32(0, 0x1d, 7, 0xFC, (1 << 29) | (1 << 17));
        ecam.or32(0, 0x1d, 7, 0xDC, (1 << 31) | (1 << 27));

        // ---- 10. Enable IOAPIC ----
        rcba.write8(OIC, 0x03);
        let _ = rcba.read8(OIC); // flush

        // ---- 11. CIR (Chipset Initialization Registers) ----
        self.setup_cir(&rcba, &ecam);

        // ---- 12. Function disable mask ----
        let fd = self.function_disable_mask();
        rcba.write32(0x3418, fd);

        fstart_log::info!("intel-ich7: early init complete (fd_mask={:#x})", fd);
        Ok(())
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
