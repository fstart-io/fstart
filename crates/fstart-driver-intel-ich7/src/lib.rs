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
        let lpc_dev = ich7::LPC_DEV;
        let lpc_func = ich7::LPC_FUNC;

        // 1. Program RCBA via ECAM.
        let rcba_lo = (self.config.rcba & 0xFFFF_C000) as u32 | 1;
        ecam.write32(0, lpc_dev, lpc_func, ich7::RCBA_REG, rcba_lo);

        // 2. LPC I/O decode (GEN1..GEN4).
        ecam.write32(0, lpc_dev, lpc_func, 0x80, self.config.lpc_decode[0]);
        ecam.write32(0, lpc_dev, lpc_func, 0x84, self.config.lpc_decode[1]);
        ecam.write32(0, lpc_dev, lpc_func, 0x88, self.config.lpc_decode[2]);
        ecam.write32(0, lpc_dev, lpc_func, 0x8C, self.config.lpc_decode[3]);

        // 3. PIRQ routing (4 bytes at 0x60, 4 bytes at 0x68).
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
        ecam.write32(0, lpc_dev, lpc_func, 0x60, pirq_low);
        ecam.write32(0, lpc_dev, lpc_func, 0x68, pirq_high);

        // 4. Function disable mask via RCBA MMIO.
        let fd = self.function_disable_mask();
        let rcba = Rcba::new((self.config.rcba & 0xFFFF_C000) as usize);
        rcba.write32(0x3418, fd);

        // 5. Enable SMBus controller via ECAM.
        let smbus = I801SmBus::enable_on_ich7(&ecam, self.config.smbus_base);
        self.smbus = Some(smbus);

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
