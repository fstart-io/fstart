//! PCI host (northbridge) early-init service.
//!
//! Implemented by drivers for x86 northbridge / PCI host controllers
//! whose registers must be unlocked (MCHBAR/DMIBAR mapping) and PAM
//! / BIOS shadow windows opened before DRAM training or PCI bus
//! enumeration can run.
//!
//! Separate from [`crate::PciRootBus`] (which covers bus enumeration
//! and BAR allocation) because early init must happen before any
//! child device is accessed, and does not expose config-space access.

use crate::ServiceError;

/// Early chipset-level initialization for the PCI host (northbridge).
///
/// Called by the `ChipsetInit` capability before `DramInit` and
/// `PciInit`. The implementation typically:
///
/// 1. Programs the MCH base address register (MCHBAR) to map
///    chipset registers into the CPU MMIO space.
/// 2. Opens PAM / BIOS shadow decode so `.rodata` and early data
///    accesses work from flash-mapped ROM.
/// 3. Enables access to PCI express config space via ECAM or
///    legacy CF8/CFC ports.
pub trait PciHost: Send + Sync {
    /// Minimal init before the console is available.
    ///
    /// Called by `ChipsetPreConsole` — the absolute minimum needed to
    /// make the console reachable. On x86 this typically enables ECAM
    /// (PCIEXBAR) so the southbridge's PCI config space is accessible.
    ///
    /// Default: no-op. Drivers that need pre-console setup override this.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Perform full early chipset initialization.
    ///
    /// Called by `ChipsetInit` after `pre_console_init` and `ConsoleInit`
    /// have completed. Must be idempotent.
    fn early_init(&mut self) -> Result<(), ServiceError>;
}
