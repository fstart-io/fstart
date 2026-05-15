//! Generic platform initialization phase service traits.
//!
//! These traits model firmware sequencing phases without encoding a
//! particular chipset topology such as northbridge/southbridge or PCH.  A
//! board stage selects the devices that participate in each phase via the
//! matching capability in the board RON.

use crate::ServiceError;

/// Minimal hardware setup required before the console can be initialized.
///
/// Examples include opening clock gates and pinmux on SoCs, enabling x86 ECAM,
/// or opening LPC decode so a SuperIO UART is reachable.
pub trait PreConsoleInit: Send + Sync {
    /// Perform pre-console setup. Implementations must not require logging.
    fn pre_console_init(&mut self) -> Result<(), ServiceError>;
}

/// Logged early platform initialization before DRAM training or bus probing.
///
/// Examples include chipset BAR setup, GPIO routing, SMBus enablement, PLL
/// setup, and other work that benefits from console diagnostics.
pub trait EarlyInit: Send + Sync {
    /// Perform early platform initialization. Must be safe to call once per
    /// stage that declares the capability.
    fn early_init(&mut self) -> Result<(), ServiceError>;
}

/// Rebuild stage-local software bindings to already-programmed hardware.
///
/// Multi-stage firmware has a fresh BSS in every stage.  Drivers can use this
/// hook to rebind global accessors, cached MMIO base addresses, or other
/// per-stage software state without repeating heavyweight hardware init.
pub trait StageLocalInit: Send + Sync {
    /// Rebuild stage-local state. Must be idempotent.
    fn stage_local_init(&mut self) -> Result<(), ServiceError>;
}

/// DRAM-backed platform/device initialization after memory is usable.
pub trait PostDramInit: Send + Sync {
    /// Perform post-DRAM setup.
    fn post_dram_init(&mut self) -> Result<(), ServiceError>;
}

/// Final platform lockdown before handing control to the payload.
pub trait FinalizeInit: Send + Sync {
    /// Lock write-once/security-sensitive hardware state.
    fn finalize_init(&mut self) -> Result<(), ServiceError>;
}
