//! Mainboard-specific early initialization hooks.
//!
//! Chipset drivers should remain reusable across boards.  Board-specific
//! routing, dock switches, SuperIO straps, and other glue live behind this
//! service and are called after chipset pre-console setup has opened the
//! required LPC/GPIO decode windows but before the console driver starts.

use crate::ServiceError;

/// Mainboard glue that must run before the console is initialized.
pub trait Mainboard: Send + Sync {
    /// Called by `ChipsetPreConsole` after `PciHost::pre_console_init()` and
    /// `Southbridge::pre_console_init()`.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}
