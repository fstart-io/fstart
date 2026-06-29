//! Device declaration types for board configuration.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// How a device physically attaches to its parent bus.
///
/// Carries just the address portion of the attachment — the parent node
/// identifies which bus type applies. For PCI devices the bus number
/// is implicit from the parent bridge; only device and function are
/// given here.
///
/// ```ron
/// ( name: "nic0", bus: Pci(0, 0), ... )
/// ( name: "superio", bus: Lpc(0x2e), ... )
/// ( name: "eeprom0", bus: I2c(0x50), ... )
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusAddress {
    /// PCI or PCIe device: (device number, function number).
    ///
    /// Bus number is implicit from the parent bridge.
    Pci(u8, u8),
    /// LPC (Low Pin Count) / ISA Plug-and-Play: config index port.
    ///
    /// Typical values: `0x2e` for primary SuperIO, `0x4e` for secondary.
    Lpc(u16),
    /// I2C / SMBus 7-bit address.
    I2c(u8),
    /// SPI chip-select index.
    Spi(u8),
    /// Plug-and-Play logical device number below a SuperIO config-port device.
    Pnp(u8),
}

fn default_enabled() -> bool {
    true
}

/// How a [`DeviceConfig`] participates in the board topology.
///
/// Rust-authored boards should use this instead of carrying fake driver
/// instances for topology-only nodes. Runtime devices bind to a real driver by
/// name. Structural roles are driverless child buses or bridge ports owned by
/// their parent device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeviceRole {
    /// Normal runtime device with a typed driver configuration.
    #[default]
    Runtime,
    /// PCI/PCIe bridge or root-port grouping downstream children.
    PciBridge,
    /// LPC bus branch below a southbridge.
    LpcBus,
    /// SMBus branch below a southbridge.
    SmBus,
    /// Generic driverless topology-only bus branch.
    GenericBus,
    /// Plug-and-Play logical device below a SuperIO chip.
    PnpDevice,
}

impl DeviceRole {
    /// Returns `true` when this role is backed by a runtime driver binding.
    pub const fn is_runtime(self) -> bool {
        matches!(self, Self::Runtime)
    }
}

/// Stable device identifier — index into the flat device table.
///
/// Maximum 256 devices per board (more than sufficient for firmware).
pub type DeviceId = u8;

/// A node in the flat, index-based device tree.
///
/// Generated into the firmware binary as a `static` table for runtime
/// introspection (power sequencing, diagnostics, etc.).  Stored in
/// topological order: a node's `parent` index is always less than its
/// own index — roots come first, then children in pre-order.
///
/// No pointers, no linked lists — just indices into a flat array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceNode {
    /// Parent device index, or `None` for root devices.
    pub parent: Option<DeviceId>,
    /// Depth in the tree (0 = root, 1 = direct child of a root, …).
    pub depth: u8,
}

impl DeviceNode {
    /// Returns `true` if this is a root device (no parent bus).
    pub const fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

/// A device declaration in the board configuration.
///
/// Carries only the identity and topology metadata for a hardware device.
/// Driver-specific configuration (register addresses, clocks, etc.) lives in
/// board-owned typed driver bindings; each driver defines its own typed `Config`
/// struct. Driver-provided services are owned by Rust driver metadata, not by
/// this board schema.
///
/// Bus hierarchies are expressed via the `parent` field: a child device
/// sets `parent` to its bus controller's name.  Codegen ensures parents
/// are initialised before children.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// Device instance name (e.g., "uart0", "flash0").
    pub name: HString<32>,
    /// Parent device name (for bus-attached devices, e.g., "i2c0").
    /// `None` for root-level devices.
    #[serde(default)]
    pub parent: Option<HString<32>>,
    /// Physical attachment to the parent bus (optional).
    ///
    /// Set for leaf devices on PCI, LPC, I2C, SMBus, or SPI buses.
    /// Absent for root devices and for internal structural nodes
    /// (LPC bus, SMBus controller) that don't present a bus address
    /// to their children.
    #[serde(default)]
    pub bus: Option<BusAddress>,
    /// Runtime device or driverless structural topology role.
    #[serde(default)]
    pub role: DeviceRole,
    /// Whether this device is enabled.
    ///
    /// Disabled devices still appear in the device tree (and in ACPI
    /// tables with `_STA` returning 0) but are not constructed or
    /// initialized by the generated code.
    ///
    /// Defaults to `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}
