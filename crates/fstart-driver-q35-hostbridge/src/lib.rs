//! Q35 PCI host bridge driver.
//!
//! This driver handles the Intel Q35 chipset's PCI root complex as
//! emulated by QEMU.  It differs from the generic [`PciEcam`] driver in
//! two ways:
//!
//! 1. **ECAM bootstrap via CF8/CFC** — on x86, the ECAM (PCIEXBAR) is
//!    not active at reset and must be programmed through legacy I/O port
//!    config access before memory-mapped config space works.
//!
//! 2. **Runtime MMIO window computation** — the 32-bit PCI MMIO hole
//!    depends on the amount of installed DRAM.  The MMIO32 window starts
//!    at `max(TOLUD, ecam_end)` and runs up to 0xFE00_0000 (below
//!    IOAPIC / LAPIC / ROM).  TOLUD is derived from the e820 memory map
//!    that `MemoryDetect` has already populated.
//!
//! The driver composes a [`PciEcam`] internally and delegates bus
//! enumeration, BAR allocation, and `PciRootBus` queries to it.
//!
//! Compatible: `"q35-hostbridge"`.

#![no_std]

extern crate alloc;

use fstart_driver_pci_ecam::{PciEcam, PciEcamConfig};
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::E820Entry;
use fstart_services::pci::{PciAddr, PciRootBus, PciWindow};
use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

/// Upper bound of the 32-bit PCI MMIO window (exclusive).
///
/// Below this are the IOAPIC (0xFEC0_0000), LAPIC (0xFEE0_0000), and
/// the flash/ROM region.  Matches coreboot's `DOMAIN_RESOURCE_32BIT_LIMIT`.
const MMIO32_LIMIT: u64 = 0xFE00_0000;

/// Fallback 64-bit MMIO limit when CPUID leaf 0x80000008 is unavailable.
/// 39-bit physical = 512 GiB — conservative for QEMU Q35.
const MMIO64_LIMIT_FALLBACK: u64 = 0x80_0000_0000;

/// Full x86 I/O port range: 0x0000..0xFFFF (64 KiB).
/// The PCI root bridge decodes the entire I/O space; legacy ISA
/// devices below 0x1000 are handled by subtractive decode.
const PIO_BASE: u64 = 0x0000;
const PIO_SIZE: u64 = 0x10000;

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the Q35 host bridge.
///
/// Only the ECAM base/size and bus range are needed.  MMIO windows are
/// computed at runtime from the e820 memory map.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Q35HostBridgeConfig {
    /// ECAM base address (PCIEXBAR).  Typically 0xB000_0000 on Q35.
    pub ecam_base: u64,
    /// ECAM region size in bytes (256 MiB for 256 buses).
    pub ecam_size: u64,
    /// First bus number in this segment.
    pub bus_start: u8,
    /// Last bus number in this segment.
    pub bus_end: u8,
}

// -----------------------------------------------------------------------
// Driver
// -----------------------------------------------------------------------

/// Q35 PCI host bridge driver.
///
/// Wraps a [`PciEcam`] with x86-specific ECAM bootstrap and
/// runtime MMIO window computation.
pub struct Q35HostBridge {
    config: Q35HostBridgeConfig,
    ecam: PciEcam,
}

// SAFETY: Same as PciEcam — hardware-fixed MMIO addresses,
// single-threaded firmware init.
unsafe impl Send for Q35HostBridge {}
unsafe impl Sync for Q35HostBridge {}

impl Q35HostBridge {
    /// Program the MCH's PCIEXBAR register via legacy CF8/CFC I/O ports.
    ///
    /// After this call, ECAM is live and all PCI config access goes
    /// through memory-mapped MMIO.
    fn enable_ecam(&self) {
        const CF8: u16 = 0xCF8;
        const CFC: u16 = 0xCFC;
        const PCIEXBAR_LO: u8 = 0x60;
        const PCIEXBAR_HI: u8 = 0x64;

        let bus_count = (self.config.bus_end as u32) - (self.config.bus_start as u32) + 1;
        let length_bits: u32 = match bus_count {
            256 => 0 << 1, // 256 MiB
            128 => 1 << 1, // 128 MiB
            64 => 2 << 1,  // 64 MiB
            _ => 0 << 1,   // default to 256
        };

        // CF8 address: (1<<31) | (bus<<16) | (dev<<11) | (fn<<8) | (reg & 0xFC)
        // Host bridge = bus 0, dev 0, fn 0.
        let cf8_hi = 0x8000_0000u32 | (PCIEXBAR_HI as u32 & 0xFC);
        let cf8_lo = 0x8000_0000u32 | (PCIEXBAR_LO as u32 & 0xFC);
        let pciexbar_val = (self.config.ecam_base as u32) | length_bits | 1;

        // SAFETY: CF8/CFC are standard x86 PCI config I/O ports.
        unsafe {
            fstart_pio::outl(CF8, cf8_hi);
            fstart_pio::outl(CFC, 0); // PCIEXBAR high = 0 (below 4 GiB)
            fstart_pio::outl(CF8, cf8_lo);
            fstart_pio::outl(CFC, pciexbar_val);
        }

        fstart_log::info!(
            "Q35: PCIEXBAR enabled at {:#x} ({} buses)",
            self.config.ecam_base,
            bus_count
        );
    }

    /// Compute TOLUD and TOUUD from e820 entries.
    ///
    /// - TOLUD: highest RAM end below 4 GiB.
    /// - TOUUD: highest RAM end (any address).  When there is no RAM
    ///   above 4 GiB, TOUUD equals 4 GiB so the MMIO64 window starts
    ///   immediately at the top of the 32-bit space.
    fn ram_tops_from_e820(entries: &[E820Entry]) -> (u64, u64) {
        let mut tolud: u64 = 0;
        let mut touud: u64 = 0x1_0000_0000; // default: 4 GiB
        for e in entries {
            // kind == 1 is E820Kind::Ram
            if e.kind == 1 {
                let top = e.addr.saturating_add(e.size);
                if top <= 0x1_0000_0000 && top > tolud {
                    tolud = top;
                }
                if top > touud {
                    touud = top;
                }
            }
        }
        (tolud, touud)
    }

    /// Configure MMIO windows from the e820 memory map and call
    /// `init()` on the inner PCI ECAM driver.
    ///
    /// This is the main entry point.  The codegen calls this instead of
    /// the `Device::init()` trait method, passing the e820 data that
    /// `MemoryDetect` has already populated.
    ///
    /// Window computation follows coreboot's Q35/i440fx pattern:
    /// - **MMIO32**: `max(TOLUD, ecam_end)` up to `0xFE00_0000`
    /// - **MMIO64**: starts above TOUUD (top of all RAM), extends to
    ///   the CPU's physical address limit
    /// - **I/O**: full 64 KiB port space (`0x0000..0xFFFF`)
    pub fn init_with_e820(&mut self, entries: &[E820Entry]) -> Result<(), DeviceError> {
        // Step 1: Enable ECAM via CF8/CFC.
        self.enable_ecam();

        // Step 2: Compute MMIO windows from memory layout.
        let (tolud, touud) = Self::ram_tops_from_e820(entries);
        let ecam_end = self.config.ecam_base + self.config.ecam_size;

        // MMIO32 starts after both DRAM and ECAM.
        let mmio32_base = core::cmp::max(tolud, ecam_end);
        let mmio32_size = MMIO32_LIMIT.saturating_sub(mmio32_base);

        // MMIO64 starts above all RAM (including high RAM above 4 GiB).
        let mmio64_base = touud;
        let mmio64_size = MMIO64_LIMIT_FALLBACK.saturating_sub(mmio64_base);

        fstart_log::info!("Q35: TOLUD={:#x} TOUUD={:#x}", tolud, touud);
        fstart_log::info!("Q35: MMIO32={:#x}..{:#x}", mmio32_base, MMIO32_LIMIT);
        fstart_log::info!(
            "Q35: MMIO64={:#x}..{:#x}",
            mmio64_base,
            mmio64_base + mmio64_size
        );

        self.ecam.configure_windows(
            mmio32_base,
            mmio32_size,
            mmio64_base,
            mmio64_size,
            PIO_BASE,
            PIO_SIZE,
        );

        // Step 3: Enumerate and allocate (delegated to PciEcam).
        self.ecam.init()
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl Device for Q35HostBridge {
    const NAME: &'static str = "q35-hostbridge";
    const COMPATIBLE: &'static [&'static str] = &["q35-hostbridge"];
    type Config = Q35HostBridgeConfig;

    fn new(config: &Q35HostBridgeConfig) -> Result<Self, DeviceError> {
        // Create the inner PciEcam with zero-sized windows.  The real
        // windows are set by init_with_e820() before enumeration.
        let ecam_config = PciEcamConfig {
            ecam_base: config.ecam_base,
            ecam_size: config.ecam_size,
            mmio32_base: 0,
            mmio32_size: 0,
            mmio64_base: 0,
            mmio64_size: 0,
            pio_base: 0,
            pio_size: 0,
            bus_start: config.bus_start,
            bus_end: config.bus_end,
        };
        let ecam = PciEcam::new(&ecam_config)?;

        Ok(Self {
            config: *config,
            ecam,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Bare init() without e820 data cannot compute windows.
        // Platform codegen should call init_with_e820() instead.
        fstart_log::error!(
            "Q35HostBridge::init() called without e820 data; \
             use init_with_e820() instead"
        );
        Err(DeviceError::InitFailed)
    }
}

// -----------------------------------------------------------------------
// PciRootBus — delegate to inner PciEcam
// -----------------------------------------------------------------------

impl PciRootBus for Q35HostBridge {
    fn config_read32(&self, addr: PciAddr, reg: u16) -> Result<u32, ServiceError> {
        self.ecam.config_read32(addr, reg)
    }

    fn config_write32(&self, addr: PciAddr, reg: u16, val: u32) -> Result<(), ServiceError> {
        self.ecam.config_write32(addr, reg, val)
    }

    fn ecam_base(&self) -> u64 {
        self.ecam.ecam_base()
    }

    fn ecam_size(&self) -> u64 {
        self.ecam.ecam_size()
    }

    fn bus_start(&self) -> u8 {
        self.ecam.bus_start()
    }

    fn bus_end(&self) -> u8 {
        self.ecam.bus_end()
    }

    fn device_count(&self) -> usize {
        self.ecam.device_count()
    }

    fn windows(&self) -> &[PciWindow] {
        self.ecam.windows()
    }
}
