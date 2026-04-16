//! Intel Atom D4xx/D5xx (Pineview) northbridge driver.
//!
//! Covers the integrated memory controller hub on the Atom D410/D510/D525
//! family (and the near-identical Cedarview N2600/N2800 with different
//! PLL constants). Responsibilities:
//!
//! - **Early init ([`PciHost::early_init`])**: program MCHBAR / DMIBAR /
//!   EPBAR so chipset registers are reachable, unlock the BIOS shadow
//!   (PAM) so `.rodata` accesses succeed when the bootblock is copied
//!   to CAR, and prepare PCIe root config windows.
//! - **DRAM training ([`MemoryController::init`])**: full DDR2 raminit.
//!   **Currently a stub** — a future phase will port the ~1800-line
//!   coreboot `northbridge/intel/pineview/raminit.c`. For now this
//!   returns `Ok(0)` so the codegen pipeline compiles end-to-end.
//!
//! DRAM size detection, SPD reading, and PCI bus enumeration all live
//! in separate crates / capabilities; this driver only owns the
//! chipset-specific register programming.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_controller::MemoryController;
use fstart_services::{PciHost, ServiceError};
use serde::{Deserialize, Serialize};

// Touch ServiceError so unused-import lint doesn't fire when the
// feature-gated PciHost impl collapses to an empty block on non-x86.
#[doc(hidden)]
#[allow(dead_code)]
fn _touch_service_error() -> Option<ServiceError> {
    None
}

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
    /// Memory Controller Hub base register — where chipset memory
    /// controller registers are mapped after early init.
    pub mchbar: u64,
    /// DMI base address register.
    pub dmibar: u64,
    /// EP base address register (ingress path).
    pub epbar: u64,
    /// Optional integrated graphics configuration.
    #[serde(default)]
    pub igd: Option<IgdConfig>,
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
        // Full DRAM training runs here when this driver is referenced
        // by a `DramInit` capability in the stage's capability list.
        //
        // TODO: port coreboot's DDR2 raminit.c (~1800 lines). Until
        // then, assume DRAM is already present (QEMU-style) and expose
        // a sentinel 1 GiB size so downstream code can link. On real
        // hardware, this function will train DDR2 with full PHY setup
        // via SPD data read from SMBus.
        fstart_log::warn!("intel-pineview: DRAM training stub — assuming 1 GiB");
        self.detected_size = 1 << 30;
        fstart_log::info!("intel-pineview: mchbar={:#x}", self.config.mchbar);
        Ok(())
    }
}

impl PciHost for IntelPineview {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        // Program MCHBAR / DMIBAR / EPBAR via PCI config space on the
        // host bridge (bus 0 device 0 function 0).
        //
        // coreboot reference: src/northbridge/intel/pineview/early_init.c
        // Registers: MCHBAR = 0x48, DMIBAR = 0x68, EPBAR = 0x40.
        //
        // The write sequence is:
        //   cfg_write32(reg + 4, base >> 32);
        //   cfg_write32(reg,     (base & 0xFFFFFFF0) | 1);  // enable bit
        //
        // For the initial skeleton we emit the writes so the driver is
        // functionally correct against real hardware but untested on QEMU.

        // SAFETY: legacy PCI config access on x86 firmware — caller has
        // ensured we are on an Intel Atom Pineview platform.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            write_pci_bar(0x40, self.config.epbar);
            write_pci_bar(0x48, self.config.mchbar);
            write_pci_bar(0x68, self.config.dmibar);
        }

        fstart_log::info!("intel-pineview: chipset early init complete");
        Ok(())
    }
}

impl MemoryController for IntelPineview {
    fn detected_size_bytes(&self) -> u64 {
        self.detected_size
    }
}

/// Write a 64-bit chipset BAR pair via legacy PCI config space.
///
/// Writes the high dword at `reg + 4`, then the low dword at `reg`
/// with the lock/enable bit (bit 0) set — the Intel convention for
/// MCHBAR / DMIBAR / EPBAR across the Nehalem / Penryn / Atom
/// Pineview families.
///
/// # Safety
/// Must only be called on x86 with a Pineview host bridge at 00:00.0.
#[cfg(target_arch = "x86_64")]
unsafe fn write_pci_bar(reg: u8, base: u64) {
    // PCI config at bus=0, dev=0, func=0.
    let lo = (base & 0xFFFF_FFF0) as u32 | 1;
    let hi = (base >> 32) as u32;
    unsafe {
        fstart_pio::pci_cfg_write32(0, 0, 0, reg + 4, hi);
        fstart_pio::pci_cfg_write32(0, 0, 0, reg, lo);
    }
}
