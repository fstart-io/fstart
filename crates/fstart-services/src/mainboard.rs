//! Mainboard-specific early initialization hooks.
//!
//! Chipset drivers should remain reusable across boards.  Board-specific
//! routing, dock switches, SuperIO straps, and other glue live behind this
//! service and are called after chipset pre-console setup has opened the
//! required LPC/GPIO decode windows but before the console driver starts.

use crate::ServiceError;

/// Mainboard glue for board-specific sequencing around reusable chipset code.
pub trait Mainboard: Send + Sync {
    /// Called by `ChipsetPreConsole` after `PciHost::pre_console_init()` and
    /// `Southbridge::pre_console_init()`.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Called during ramstage/late-driver init, after DRAM and generic device
    /// construction are available. Boards use this for EC, dock, mux, and
    /// board-local PCI quirks that do not belong in reusable chipset drivers.
    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Called during final chipset lockdown. Default is no-op.
    fn finalize(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}
