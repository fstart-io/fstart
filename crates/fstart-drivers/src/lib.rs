//! Hardware driver implementations.
//!
//! Each driver is feature-gated so only the drivers a board needs are compiled.
//! In rigid mode, unused drivers are completely eliminated.

#![no_std]

pub mod uart;

use fstart_types::device::Resources;

/// Error constructing or using a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// A required resource was missing
    MissingResource(&'static str),
    /// Hardware initialization failed
    InitFailed,
}

/// Trait that all drivers implement for identification and construction.
pub trait Driver: Send + Sync {
    /// Driver name (e.g., "ns16550")
    const NAME: &'static str;
    /// Compatible strings this driver handles
    const COMPATIBLE: &'static [&'static str];
    /// Construct from hardware resources
    fn from_resources(resources: &Resources) -> Result<Self, DriverError>
    where
        Self: Sized;
}
