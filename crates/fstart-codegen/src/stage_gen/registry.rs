//! Driver registry — helpers used by topology validation.
//!
//! With the typed [`fstart_device_registry::DriverInstance`] enum,
//! most driver metadata is reached via `instance.meta()`.  This module
//! now only hosts the bus-service name list plus the
//! [`is_bus_provider`] predicate consumed by
//! [`super::topology::validate_device_tree`].

use fstart_types::DeviceConfig;

/// Bus service names that indicate a device is a bus controller.
///
/// A parent device must provide at least one of these for a **bus-device**
/// child (`is_bus_device == true`) to be accepted by
/// [`super::topology::validate_device_tree`].  Plain-device children
/// (e.g., an NS16550 UART nested under a SuperIO for init ordering)
/// don't require the parent to provide any of these.
///
/// The list covers:
/// - Real bus controllers (`I2cBus`, `SpiBus`, `PciRootBus`).
/// - GPIO controllers that expose pins as children.
/// - x86 chipset sub-hierarchies: the northbridge (`PciHost`),
///   southbridge (`Southbridge`), PCIe root ports (`PciBridge`),
///   LPC bus (`LpcBus`), SMBus (`SmBus`), and SuperIO hosts
///   (`SuperIoHost`).
const BUS_SERVICES: &[&str] = &[
    "I2cBus",
    "SpiBus",
    "GpioController",
    "PciRootBus",
    "PciHost",
    "Southbridge",
    "PciBridge",
    "LpcBus",
    "SmBus",
    "SuperIoHost",
];

/// Returns true if a device provides a bus service.
pub(super) fn is_bus_provider(dev: &DeviceConfig) -> bool {
    dev.services
        .iter()
        .any(|s| BUS_SERVICES.contains(&s.as_str()))
}
