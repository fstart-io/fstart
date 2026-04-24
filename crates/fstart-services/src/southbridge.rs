//! Southbridge early-init service.
//!
//! Implemented by drivers for x86 southbridge / PCH chipsets that
//! need their Root Complex Base Address (RCBA), LPC decode windows,
//! and GPIO/GPE configuration programmed before the firmware can use
//! the console UART (often behind SuperIO on LPC) or other SB-attached
//! devices.

use crate::ServiceError;

/// Early chipset-level initialization for the southbridge.
///
/// Called by the `ChipsetInit` capability. The implementation typically:
///
/// 1. Programs RCBA and maps RCBA MMIO.
/// 2. Opens LPC I/O / memory decode ranges so the SuperIO UART is
///    reachable (the pre-console portion, split into [`pre_console_init`]).
/// 3. Programs GPIO/GPE routing registers.
/// 4. Disables integrated functions listed as `false` in the config
///    (HD audio, PATA, unused PCIe ports, ...) via the Function
///    Disable register.
pub trait Southbridge: Send + Sync {
    /// Minimal init before the console is available.
    ///
    /// Called by `ChipsetPreConsole` — opens LPC decode ranges so
    /// the SuperIO (sitting on the LPC bus at 0x2E/0x2F or 0x4E/0x4F)
    /// is reachable for COM port programming.
    ///
    /// Default: no-op. Drivers that need pre-console setup override this.
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Perform full early southbridge initialization.
    fn early_init(&mut self) -> Result<(), ServiceError>;
}
