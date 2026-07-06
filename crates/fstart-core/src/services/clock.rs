//! Clock controller service trait.
//!
//! Implemented by SoC clock tree drivers (CCU, PLL controllers).
//! Board/platform fixed-flow steps perform the initial clock tree setup.
//! This trait provides runtime clock management for other drivers that need
//! to enable/disable clock gates or query frequencies.

use super::ServiceError;

/// Clock controller — manages PLL configuration and peripheral clock gates.
///
/// Board/platform clock-init steps program PLLs and set up default clock
/// dividers. `ClockController` methods enable individual drivers to open
/// their clock gates and query bus frequencies at runtime.
///
/// # Note
///
/// For the initial boot stage (SRAM-only), only the fixed-flow clock step
/// should run. The full `ClockController` interface is available to later
/// stages running from DRAM.
pub trait ClockController: Send + Sync {
    /// Enable the clock gate for a peripheral identified by `gate_id`.
    ///
    /// Gate IDs are SoC-specific.  On Allwinner A20, these correspond
    /// to bit positions in the AHB/APB gate registers.
    fn enable_clock(&self, gate_id: u32) -> Result<(), ServiceError>;

    /// Disable the clock gate for a peripheral.
    fn disable_clock(&self, gate_id: u32) -> Result<(), ServiceError>;

    /// Query the frequency (in Hz) of a clock identified by `clock_id`.
    ///
    /// Returns `NotSupported` if the clock ID is unknown.
    fn get_frequency(&self, clock_id: u32) -> Result<u32, ServiceError>;
}
