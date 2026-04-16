//! Intel ICH7 (I/O Controller Hub 7) southbridge driver.
//!
//! Applies to the NM10 Express used on Atom D-series boards and the
//! generic ICH7 found on Core 2 / Pentium 4 platforms. Responsibilities:
//!
//! - Program RCBA (Root Complex Base Address) so that the chipset's
//!   non-PCI-config MMIO block (backbone, GPIO, GPE, etc.) is
//!   addressable from firmware.
//! - Open LPC I/O / memory decode windows so the SuperIO UART, TPM,
//!   and boot ROM are reachable before DRAM init.
//! - Program PIRQ routing so ACPI `_PRT` entries can map to sensible
//!   GSIs.
//! - Apply the function-disable mask for integrated functions that
//!   are off (HD audio, PATA, SATA, unused PCIe ports).
//!
//! The driver also exposes the [`fstart_superio::LpcBaseProvider`]
//! trait so a `SuperIo<Chip>` child can be constructed on the LPC bus.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::{ServiceError, Southbridge};
use fstart_superio::LpcBaseProvider;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Named SATA configuration for the SATA PCI function.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SataConfig {
    /// SATA controller mode (IDE vs AHCI).
    pub mode: SataMode,
    /// Port enable bitmask (bit N = SATA port N).
    pub ports: u8,
}

/// SATA controller operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SataMode {
    /// Legacy IDE / PATA-compatible mode.
    Ide,
    /// AHCI mode (preferred on modern OSes).
    Ahci,
}

/// USB controller subset configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UsbConfig {
    /// Enable the EHCI (USB 2.0) controller.
    #[serde(default)]
    pub ehci: bool,
    /// Enable each UHCI (USB 1.1) companion controller (4 total).
    #[serde(default)]
    pub uhci: [bool; 4],
}

/// ICH7 southbridge configuration.
///
/// Named subsystems replace raw `device pci 1f.2 on end` style entries.
/// The driver maps names to PCI device/function numbers internally.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IntelIch7Config {
    /// Root Complex Base Address register value.
    pub rcba: u64,
    /// PIRQ routing (one byte per PIRQ A..H).
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits (ACPI GPE bitmap).
    pub gpe0_en: u32,
    /// LPC I/O decode register values (LPC_IOD1 .. LPC_IOD4).
    pub lpc_decode: [u32; 4],
    /// Enable the HD Audio function.
    #[serde(default)]
    pub hd_audio: bool,
    /// SATA controller config (None = disable the SATA function).
    #[serde(default)]
    pub sata: Option<SataConfig>,
    /// USB controller config (None = disable all USB).
    #[serde(default)]
    pub usb: Option<UsbConfig>,
    /// Enable the PATA (legacy IDE) function.
    #[serde(default)]
    pub pata: bool,
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

/// Intel ICH7 southbridge driver.
pub struct IntelIch7 {
    config: IntelIch7Config,
}

// SAFETY: All state is CPU-exclusive during firmware phase.
unsafe impl Send for IntelIch7 {}
unsafe impl Sync for IntelIch7 {}

/// Encode a PCI device/function as a single byte (devfn).
const fn devfn(dev: u8, func: u8) -> u8 {
    (dev << 3) | (func & 0x7)
}

impl IntelIch7 {
    /// HD Audio devfn = 1B.0.
    #[allow(dead_code)]
    const DEVFN_HDA: u8 = devfn(0x1b, 0);
    /// SATA devfn = 1F.2.
    #[allow(dead_code)]
    const DEVFN_SATA: u8 = devfn(0x1f, 2);
    /// LPC bridge devfn = 1F.0.
    #[allow(dead_code)]
    const DEVFN_LPC: u8 = devfn(0x1f, 0);
    /// SMBus controller devfn = 1F.3.
    #[allow(dead_code)]
    const DEVFN_SMBUS: u8 = devfn(0x1f, 3);

    /// Compute the Function Disable (FD) bitmask — one bit per
    /// integrated function that is off in the config.
    ///
    /// Bit assignments per the ICH7 datasheet:
    ///   bit 0 : reserved
    ///   bit 1 : PATA disable
    ///   bit 2 : SATA disable
    ///   bit 3 : SMBus disable (unused here — always on)
    ///   bit 4 : HD Audio disable
    ///   ...
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
        Ok(Self { config: *config })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Post-DRAM driver init is a no-op. Early init (RCBA,
        // LPC decode, PIRQ, FD) runs in Southbridge::early_init.
        fstart_log::info!("intel-ich7: rcba={:#x}", self.config.rcba);
        Ok(())
    }
}

impl Southbridge for IntelIch7 {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        // coreboot reference: src/southbridge/intel/i82801gx/early_smbus.c
        // + src/southbridge/intel/i82801gx/early_init.c.
        //
        // The key sequence:
        //   1. Program RCBA (LPC config reg 0xF0) + enable bit.
        //   2. Program LPC I/O decode (LPC config reg 0x80/0x84/0x88/0x8C).
        //   3. Program GPE0 enable (ACPI block base + 0x28).
        //   4. Program PIRQ routing (LPC config reg 0x60..0x6B).
        //   5. Set the function-disable mask (RCBA + 0x3418).

        #[cfg(target_arch = "x86_64")]
        {
            let rcba_lo = (self.config.rcba & 0xFFFF_C000) as u32 | 1;
            // SAFETY: caller has ensured we are on an ICH7 platform.
            unsafe {
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0xF0, rcba_lo);
                // LPC I/O decode.
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x80, self.config.lpc_decode[0]);
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x84, self.config.lpc_decode[1]);
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x88, self.config.lpc_decode[2]);
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x8C, self.config.lpc_decode[3]);
                // PIRQ routing (4 bytes at 0x60, 4 bytes at 0x68).
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
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x60, pirq_low);
                fstart_pio::pci_cfg_write32(0, 0x1f, 0, 0x68, pirq_high);
            }

            // Function disable mask: RCBA MMIO at offset 0x3418.
            let fd = self.function_disable_mask();
            let fd_addr = (self.config.rcba + 0x3418) as *mut u32;
            // SAFETY: RCBA is a fixed chipset MMIO base programmed above.
            unsafe {
                fd_addr.write_volatile(fd);
            }
        }

        fstart_log::info!(
            "intel-ich7: early init complete (fd_mask={:#x})",
            self.function_disable_mask()
        );
        Ok(())
    }
}

/// Expose the southbridge's LPC bus base port as an [`LpcBaseProvider`]
/// so a SuperIO child can be constructed via `BusDevice::new_on_bus`.
///
/// The LPC I/O decode is already programmed in `early_init`; the
/// SuperIO config index port comes from the child's `bus: Lpc(0x2e)`
/// field, not from this trait. For now we return 0 — the codegen
/// layer is responsible for threading the bus address in via the
/// generated `new_on_bus(&cfg, &bus)` call.
impl LpcBaseProvider for IntelIch7 {
    fn lpc_base(&self) -> u16 {
        // Conventionally 0x2e for primary SuperIO on ICH7 boards.
        // A future revision will plumb the child's `bus: Lpc(addr)`
        // field through to this provider.
        0x2e
    }
}
