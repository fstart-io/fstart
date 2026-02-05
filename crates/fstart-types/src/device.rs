//! Device and driver binding types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// A device declaration in the board configuration.
/// Maps a hardware device to a driver and one or more service traits.
///
/// Bus hierarchies are expressed via the `parent` field: a child device
/// sets `parent` to its bus controller's name.  Codegen ensures parents
/// are initialised before children.
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
    /// Parent device name (for bus-attached devices, e.g., "i2c0").
    /// `None` for root-level devices.
    #[serde(default)]
    pub parent: Option<HString<32>>,
}

/// Hardware resources for a device.
///
/// This is the RON interchange format — deliberately flat and permissive
/// (all fields `Option`).  Codegen maps these to driver-specific typed
/// `Config` structs at build time.
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
    /// Bus speed in Hz (e.g., 100000 for I2C standard, 400000 for fast)
    pub bus_speed: Option<u32>,
}
