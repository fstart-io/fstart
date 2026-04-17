//! SG2042 DDR memory controller driver — stub pending RE.
//!
//! The production DDR init code (`mango_ddr_init_asic`) is a ~100 KB
//! closed-source blob. This stub returns `Err(InitFailed)` with a clear
//! diagnostic message, causing the `DramInit` capability to halt the system.
//!
//! When DDR init is reverse-engineered and reimplemented, only this file
//! changes — the board RON and capability sequence remain the same.

use serde::{Deserialize, Serialize};

use fstart_services::{
    device::{Device, DeviceError},
    memory_controller::MemoryController,
};

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 DDR controller (stub — no fields yet).
///
/// When the real implementation is added, this struct will carry DDR
/// channel count, DIMM SPD address, training parameters, etc.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042DdrConfig {}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 DDR memory controller (stub).
///
/// Returns `Err(InitFailed)` immediately — DDR init is pending
/// reverse engineering of the closed-source Sophgo blob.
pub struct Sg2042Ddr;

// SAFETY: no mutable state; all methods are pure error returns.
unsafe impl Send for Sg2042Ddr {}
unsafe impl Sync for Sg2042Ddr {}

impl Device for Sg2042Ddr {
    const NAME: &'static str = "sg2042-ddr";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-ddr"];
    type Config = Sg2042DdrConfig;

    fn new(_config: &Sg2042DdrConfig) -> Result<Self, DeviceError> {
        Ok(Sg2042Ddr)
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        fstart_log::error!(
            "[SG2042 DDR] not yet implemented \
             — reverse engineering of mango_ddr_init_asic pending; \
             system halting"
        );
        Err(DeviceError::InitFailed)
    }
}

impl MemoryController for Sg2042Ddr {
    fn detected_size_bytes(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ddr_init_returns_err() {
        let mut ddr = Sg2042Ddr::new(&Sg2042DdrConfig {}).unwrap();
        assert!(matches!(ddr.init(), Err(DeviceError::InitFailed)));
    }
}
