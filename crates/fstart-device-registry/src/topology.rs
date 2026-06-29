use crate::{DriverInstance, DriverInstanceBinding};
use heapless::String as HString;
use std::vec::Vec;

/// Board-supplied runtime device to attach to a platform-owned topology point./// Board-supplied runtime device to attach to a platform-owned topology point.
///
/// Platform crates own the canonical chipset skeleton, while board crates add
/// devices at named extension points such as an LPC bus, SMBus, or PCIe root
/// port.  This helper keeps the runtime device declaration and its driver
/// binding together so board/platform code cannot forget one side.
#[derive(Debug, Clone)]
pub struct PlatformRuntimeDevice {
    /// Runtime device name in the flattened board topology.
    pub name: HString<32>,
    /// Parent node supplied by the platform topology template.
    pub parent: HString<32>,
    /// Physical attachment below the parent.
    pub bus: fstart_types::BusAddress,
    /// Whether the board declares this device present/enabled.
    pub enabled: bool,
    /// Typed runtime driver configuration for this device.
    pub instance: DriverInstance,
}

/// Reusable collection of board additions for platform topology templates.
#[derive(Debug, Clone, Default)]
pub struct PlatformDeviceExtensions {
    runtime_devices: Vec<PlatformRuntimeDevice>,
}

/// Scoped builder for one platform-owned attachment point.
pub struct PlatformAttachPoint<'a> {
    parent: &'static str,
    extensions: &'a mut PlatformDeviceExtensions,
}

/// Platform topology plus the driver bindings produced while authoring it.
#[derive(Debug, Clone)]
pub struct PlatformTopology {
    topology: fstart_types::DeviceTopology,
    bindings: Vec<DriverInstanceBinding>,
}

impl PlatformDeviceExtensions {
    /// Create an empty extension set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Author board devices below a platform-owned parent node.
    pub fn on<F>(&mut self, parent: &'static str, extend: F)
    where
        F: FnOnce(&mut PlatformAttachPoint<'_>),
    {
        let mut point = PlatformAttachPoint {
            parent,
            extensions: self,
        };
        extend(&mut point);
    }

    fn iter(&self) -> impl Iterator<Item = &PlatformRuntimeDevice> {
        self.runtime_devices.iter()
    }
}

impl<'a> PlatformAttachPoint<'a> {
    /// Add a runtime child with an explicit attachment address.
    pub fn runtime(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        instance: DriverInstance,
    ) -> &mut Self {
        self.runtime_enabled(name, bus, true, instance)
    }

    /// Add a runtime child with an explicit enabled policy.
    pub fn runtime_enabled(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        instance: DriverInstance,
    ) -> &mut Self {
        self.extensions.runtime_devices.push(PlatformRuntimeDevice {
            name: fstart_types::hstr(name),
            parent: fstart_types::hstr(self.parent),
            bus,
            enabled,
            instance,
        });
        self
    }

    /// Add a PCI child below this attachment point.
    pub fn pci(
        &mut self,
        name: &str,
        device: u8,
        function: u8,
        instance: DriverInstance,
    ) -> &mut Self {
        self.runtime(
            name,
            fstart_types::BusAddress::Pci(device, function),
            instance,
        )
    }

    /// Add an LPC child below this attachment point.
    pub fn lpc(&mut self, name: &str, config_port: u16, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::Lpc(config_port), instance)
    }

    /// Add an SMBus/I2C-addressed child below this attachment point.
    pub fn i2c(&mut self, name: &str, address: u8, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::I2c(address), instance)
    }

    /// Add an SPI child below this attachment point.
    pub fn spi(&mut self, name: &str, chip_select: u8, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::Spi(chip_select), instance)
    }
}

impl PlatformTopology {
    /// Start an empty platform topology template.
    #[must_use]
    pub fn new() -> Self {
        Self {
            topology: fstart_types::DeviceTopology::new(),
            bindings: Vec::new(),
        }
    }

    /// Add a root runtime device and bind its driver in one step.
    #[must_use]
    pub fn root(mut self, name: &str, instance: DriverInstance) -> Self {
        self.topology = self.topology.root(name);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Add a root runtime device with an explicit enabled policy and bind its driver.
    #[must_use]
    pub fn root_enabled(mut self, name: &str, enabled: bool, instance: DriverInstance) -> Self {
        self.topology = self.topology.runtime_root(name, enabled);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Add a driverless child bus owned by a platform device.
    #[must_use]
    pub fn child_bus(mut self, parent: &str, name: &str, role: fstart_types::DeviceRole) -> Self {
        self.topology = self.topology.child_bus(parent, name, role);
        self
    }

    /// Add a driverless PCI/PCIe bridge or root-port node.
    #[must_use]
    pub fn pci_bridge(
        mut self,
        parent: &str,
        name: &str,
        device: u8,
        function: u8,
        enabled: bool,
    ) -> Self {
        self.topology = self
            .topology
            .pci_bridge(parent, name, device, function, enabled);
        self
    }

    /// Add a runtime child and bind its driver in one step.
    #[must_use]
    pub fn runtime(
        mut self,
        parent: &str,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        instance: DriverInstance,
    ) -> Self {
        self.topology = self.topology.runtime_child(parent, name, bus, enabled);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Apply board-supplied runtime devices to this platform topology.
    #[must_use]
    pub fn extend(mut self, extensions: &PlatformDeviceExtensions) -> Self {
        for device in extensions.iter() {
            self = self.runtime(
                device.parent.as_str(),
                device.name.as_str(),
                device.bus,
                device.enabled,
                device.instance.clone(),
            );
        }
        self
    }

    /// Finish as the flattened runtime device table.
    #[must_use]
    pub fn build_devices(self) -> heapless::Vec<fstart_types::DeviceConfig, 32> {
        self.topology.build()
    }

    /// Finish as flattened devices and matching driver bindings.
    #[must_use]
    pub fn build(
        self,
    ) -> (
        heapless::Vec<fstart_types::DeviceConfig, 32>,
        Vec<DriverInstanceBinding>,
    ) {
        (self.topology.build(), self.bindings)
    }
}

impl Default for PlatformTopology {
    fn default() -> Self {
        Self::new()
    }
}
