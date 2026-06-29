//! Generic platform initialization phase service traits.
//!
//! These traits model firmware sequencing phases without encoding a
//! particular chipset topology such as northbridge/southbridge or PCH.  A
//! board stage selects the devices that participate in each phase via Rust
//! board/build metadata and flow profiles.

use core::marker::PhantomData;

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

/// Context shared across step-based hardware initialization methods.
///
/// The initial context is deliberately small. Board-owned stage adapters can
/// extend this with generic device lookup and stage-local services without
/// adding chipset-specific helpers to common code.
pub struct InitContext<'stage> {
    _stage: PhantomData<&'stage mut ()>,
}

impl<'stage> InitContext<'stage> {
    /// Construct an empty initialization context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            _stage: PhantomData,
        }
    }
}

impl Default for InitContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Step-based hardware initialization lifecycle.
///
/// Drivers implement only the steps where they participate. Default no-op
/// methods make static typed board containers cheap: monomorphized release
/// builds can inline and remove calls to steps a concrete driver does not use.
pub trait HardwareInit {
    /// Run board/SoC operations that must happen before clocks or console.
    #[inline(always)]
    fn very_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Enable clocks/resets needed by pinmux, UART, timers, and DRAM setup.
    #[inline(always)]
    fn early_clocks(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Route pins for early UART, DRAM, boot media, and board straps.
    #[inline(always)]
    fn pinmux(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Decode/register setup required before a console driver can work.
    #[inline(always)]
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Initialize the selected boot console.
    #[inline(always)]
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run diagnostics and safety checks after logging is available.
    #[inline(always)]
    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Read SPD, straps, or board config needed for memory init.
    #[inline(always)]
    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Train/enable DRAM or validate fixed RAM setup.
    #[inline(always)]
    fn dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run DRAM-backed platform/device setup.
    #[inline(always)]
    fn post_dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Enable fixed bridges/resources before probing children.
    #[inline(always)]
    fn bus_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Discover or instantiate child devices on enumerable buses.
    #[inline(always)]
    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run hooks after generic drivers are available.
    #[inline(always)]
    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Bring up boot media and firmware-volume access.
    #[inline(always)]
    fn storage(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Measure or verify firmware and policy inputs.
    #[inline(always)]
    fn security(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Let devices participate in payload loading.
    #[inline(always)]
    fn payload_load(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Finalize handoff tables and quiesce firmware-owned devices.
    #[inline(always)]
    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl HardwareInit for () {}
