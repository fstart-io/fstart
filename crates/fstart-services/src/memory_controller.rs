//! Memory controller service trait.
//!
//! Implemented by DRAM controller drivers.  The `Device::init()` method
//! performs the full DRAM initialization sequence (PLL setup, PHY
//! training, size detection).  This trait exposes detected parameters
//! for use by later firmware stages.

use crate::ServiceError;

/// Memory controller — DRAM initialization and detection.
///
/// `Device::init()` runs the full hardware init sequence:
/// - Program the DRAM PLL
/// - Configure controller timing parameters
/// - Reset the DRAM chips (DDR3 reset sequence)
/// - Run DQS gate training / read calibration
/// - Detect installed DRAM size
///
/// After `init()` succeeds, `detected_size_bytes()` returns the usable
/// DRAM capacity.
pub trait MemoryController: Send + Sync {
    /// Return the detected DRAM size in bytes.
    ///
    /// Only valid after `Device::init()` has completed successfully.
    /// Returns 0 if DRAM init failed or has not been run.
    fn detected_size_bytes(&self) -> u64;

    /// Perform a basic memory test (optional, default no-op).
    ///
    /// Writes and reads back a pattern at several addresses to verify
    /// DRAM is functional.  Returns `HardwareError` if the test fails.
    fn memory_test(&self) -> Result<(), ServiceError> {
        Ok(())
    }
}
