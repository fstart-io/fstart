//! Network interface service.
//!
//! Minimal surface area for firmware-phase NIC drivers: expose the
//! hardware MAC address for ACPI/FDT handoff and SMBIOS Type 41.
//! Actual packet send/receive is out of scope for firmware.

use crate::ServiceError;

/// Ethernet / wireless network interface.
pub trait Network: Send + Sync {
    /// Return the hardware MAC address (48 bits, big-endian bytes).
    ///
    /// Returns `ServiceError::HardwareError` if the MAC cannot be read
    /// (e.g., NIC not responding, EEPROM read failure).
    fn mac_address(&self) -> Result<[u8; 6], ServiceError>;
}
