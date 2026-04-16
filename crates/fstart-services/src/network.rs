//! Network interface service.
//!
//! Minimal surface area for firmware-phase NIC drivers: expose the
//! hardware MAC address for ACPI/FDT handoff and SMBIOS Type 41.
//! Actual packet send/receive is out of scope for firmware.

/// Ethernet / wireless network interface.
pub trait Network: Send + Sync {
    /// Return the hardware MAC address (48 bits, big-endian bytes).
    fn mac_address(&self) -> [u8; 6];
}
