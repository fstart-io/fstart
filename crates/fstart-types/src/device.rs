//! Device and driver binding types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// A device declaration in the board configuration.
/// Maps a hardware device to a driver and one or more service traits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Device instance name (e.g., "uart0", "flash0")
    pub name: HString<32>,
    /// Compatible string for identification (e.g., "ns16550a")
    pub compatible: HString<64>,
    /// Driver name to use (e.g., "ns16550", "pl011")
    pub driver: HString<32>,
    /// Which service traits this device provides (e.g., ["Console"])
    pub services: heapless::Vec<HString<32>, 8>,
    /// Hardware resources (addresses, clocks, etc.)
    pub resources: Resources,
}

/// Hardware resources for a device.
/// Passed to the driver constructor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Resources {
    /// Memory-mapped I/O base address
    pub mmio_base: Option<u64>,
    /// I/O port base address (x86-style)
    pub io_base: Option<u16>,
    /// Region size in bytes
    pub size: Option<u64>,
    /// Clock frequency in Hz
    pub clock_freq: Option<u32>,
    /// Baud rate (for UARTs)
    pub baud_rate: Option<u32>,
    /// Interrupt number
    pub irq: Option<u32>,
    /// Bus address (e.g., I2C address, SPI chip select)
    pub bus_addr: Option<u32>,
}
