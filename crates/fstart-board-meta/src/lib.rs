//! Board-owned driver metadata and topology helpers.
//!
//! This crate is the neutral replacement for the old device registry's common
//! metadata pieces. It intentionally has no dependency on concrete driver crates:
//! boards choose driver crates directly, and each driver can implement
//! [`BoardDriver`] for its own config type.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unexpected_cfgs)]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Debug;

use heapless::String as HString;
use serde::{Deserialize, Serialize};

pub use fstart_services::{ServiceKind, ServiceSet};

/// Typed topology role for structural (driverless) device tree nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuralKind {
    /// PCI bridge/port grouping children below a PCI root or host.
    PciBridge,
    /// LPC bus branch below a southbridge.
    LpcBus,
    /// SMBus branch below a southbridge.
    SmBus,
    /// Generic topology-only bus branch.
    GenericBus,
    /// Plug-and-Play logical device below a SuperIO chip.
    PnpDevice,
}

/// Configuration for structural (driverless) device tree nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralConfig {
    /// Board-owned topology role for this structural node.
    pub kind: StructuralKind,
}

impl Default for StructuralConfig {
    fn default() -> Self {
        Self {
            kind: StructuralKind::GenericBus,
        }
    }
}

/// Build-time metadata supplied by configured board drivers.
///
/// This trait intentionally describes only generic driver facts. Platform,
/// chipset, firmware-image, flash-layout, and packaging policy belongs in
/// board/platform build metadata, not on individual driver configs.
pub trait BoardDriver: Debug + Send + Sync + 'static {
    /// Cargo feature that enables this driver in `fstart-stage`.
    fn feature(&self) -> &'static str;

    /// Runtime services this configured driver provides.
    fn services(&self) -> ServiceSet;

    /// Optional board-relative files this configured driver needs packaged.
    fn package_files(&self) -> Vec<DriverPackageFile> {
        Vec::new()
    }

    /// Clone this configured driver behind a trait object.
    fn clone_box(&self) -> Box<dyn BoardDriver>;
}

/// Board-relative file required by a configured driver at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverPackageFile {
    /// Logical file name inside the firmware package.
    pub name: HString<128>,
    /// Board-relative source path.
    pub path: HString<128>,
}

impl DriverPackageFile {
    #[must_use]
    pub fn new(name: &str, path: &str) -> Self {
        Self {
            name: fstart_types::hstr(name),
            path: fstart_types::hstr(path),
        }
    }
}

impl Clone for Box<dyn BoardDriver> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

impl BoardDriver for Box<dyn BoardDriver> {
    fn feature(&self) -> &'static str {
        self.as_ref().feature()
    }

    fn services(&self) -> ServiceSet {
        self.as_ref().services()
    }

    fn package_files(&self) -> Vec<DriverPackageFile> {
        self.as_ref().package_files()
    }

    fn clone_box(&self) -> Box<dyn BoardDriver> {
        self.as_ref().clone_box()
    }
}

/// Typed runtime driver configuration bound to a board device name.
#[derive(Debug, Clone)]
pub struct DriverBinding {
    /// Device name in the board's flat topology table.
    pub device: HString<32>,
    /// Board-owned driver metadata for that device.
    pub driver: Box<dyn BoardDriver>,
}

/// Serializable driver metadata fact emitted by board host metadata binaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverFact {
    /// Device name in the board's flat topology table.
    pub device: HString<32>,
    /// Cargo feature that enables this driver in the selected stage binary.
    pub feature: HString<64>,
    /// Runtime services this configured driver provides.
    pub services: ServiceSet,
    /// Optional board-relative files this configured driver needs packaged.
    pub package_files: Vec<DriverPackageFile>,
}

impl DriverFact {
    /// Convert a typed in-process driver binding into a serializable host fact.
    #[must_use]
    pub fn from_binding(binding: &DriverBinding) -> Self {
        Self {
            device: binding.device.clone(),
            feature: fstart_types::hstr(binding.driver.feature()),
            services: binding.driver.services(),
            package_files: binding.driver.package_files(),
        }
    }
}

/// Serializable board metadata emitted by board crates for host tooling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBoardMetadata {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: fstart_types::BoardConfig,
    /// Build/package metadata consumed by xtask.
    pub build_info: fstart_types::BuildInfo,
    /// Erased runtime driver facts keyed by board device name.
    pub drivers: Vec<DriverFact>,
    /// ACPI-only descriptors that do not participate in runtime topology.
    pub acpi_only_devices: Vec<fstart_types::acpi::AcpiExtraDevice>,
}

impl HostBoardMetadata {
    /// Construct serializable host metadata from board-owned typed bindings.
    #[must_use]
    pub fn from_bindings(
        config: fstart_types::BoardConfig,
        build_info: fstart_types::BuildInfo,
        bindings: Vec<DriverBinding>,
        acpi_only_devices: Vec<fstart_types::acpi::AcpiExtraDevice>,
    ) -> Self {
        Self {
            config,
            build_info,
            drivers: bindings.iter().map(DriverFact::from_binding).collect(),
            acpi_only_devices,
        }
    }
}

impl DriverBinding {
    /// Bind a driver metadata object to a board device name.
    #[must_use]
    pub fn new(device: &str, driver: Box<dyn BoardDriver>) -> Self {
        let mut name = HString::new();
        name.push_str(device)
            .expect("driver binding device name exceeds capacity");
        Self {
            device: name,
            driver,
        }
    }

    /// Cargo feature for this binding's driver.
    #[must_use]
    pub fn driver_feature(&self) -> &'static str {
        self.driver.feature()
    }
}

/// Convenience extension implemented for all cloneable board driver configs.
pub trait BindDriver: BoardDriver + Clone + Sized {
    /// Bind this configured driver to a board device name.
    #[must_use]
    fn bind(self, device: &str) -> DriverBinding {
        DriverBinding::new(device, Box::new(self))
    }
}

impl<T> BindDriver for T where T: BoardDriver + Clone + Sized {}

/// Board-supplied runtime device to attach to a platform-owned topology point.
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
    /// Typed runtime driver metadata for this device.
    pub driver: Box<dyn BoardDriver>,
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
    bindings: Vec<DriverBinding>,
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
    pub fn runtime<D>(&mut self, name: &str, bus: fstart_types::BusAddress, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime_enabled(name, bus, true, driver)
    }

    /// Add a runtime child with an explicit enabled policy.
    pub fn runtime_enabled<D>(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        driver: D,
    ) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.extensions.runtime_devices.push(PlatformRuntimeDevice {
            name: fstart_types::hstr(name),
            parent: fstart_types::hstr(self.parent),
            bus,
            enabled,
            driver: Box::new(driver),
        });
        self
    }

    /// Add a PCI child below this attachment point.
    pub fn pci<D>(&mut self, name: &str, device: u8, function: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime(
            name,
            fstart_types::BusAddress::Pci(device, function),
            driver,
        )
    }

    /// Add an LPC child below this attachment point.
    pub fn lpc<D>(&mut self, name: &str, config_port: u16, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime(name, fstart_types::BusAddress::Lpc(config_port), driver)
    }

    /// Add an SMBus/I2C-addressed child below this attachment point.
    pub fn i2c<D>(&mut self, name: &str, address: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime(name, fstart_types::BusAddress::I2c(address), driver)
    }

    /// Add an SPI child below this attachment point.
    pub fn spi<D>(&mut self, name: &str, chip_select: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime(name, fstart_types::BusAddress::Spi(chip_select), driver)
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
    pub fn root<D>(mut self, name: &str, driver: D) -> Self
    where
        D: BoardDriver + Clone,
    {
        self.topology = self.topology.root(name);
        self.bindings.push(driver.bind(name));
        self
    }

    /// Add a root runtime device with an explicit enabled policy and bind its driver.
    #[must_use]
    pub fn root_enabled<D>(mut self, name: &str, enabled: bool, driver: D) -> Self
    where
        D: BoardDriver + Clone,
    {
        self.topology = self.topology.runtime_root(name, enabled);
        self.bindings.push(driver.bind(name));
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
    pub fn runtime<D>(
        mut self,
        parent: &str,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        driver: D,
    ) -> Self
    where
        D: BoardDriver + Clone,
    {
        self.topology = self.topology.runtime_child(parent, name, bus, enabled);
        self.bindings.push(driver.bind(name));
        self
    }

    /// Apply board-supplied runtime devices to this platform topology.
    #[must_use]
    pub fn extend(mut self, extensions: &PlatformDeviceExtensions) -> Self {
        for device in extensions.iter() {
            self.topology = self.topology.runtime_child(
                device.parent.as_str(),
                device.name.as_str(),
                device.bus,
                device.enabled,
            );
            self.bindings.push(DriverBinding::new(
                device.name.as_str(),
                device.driver.clone(),
            ));
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
        Vec<DriverBinding>,
    ) {
        (self.topology.build(), self.bindings)
    }
}

impl Default for PlatformTopology {
    fn default() -> Self {
        Self::new()
    }
}
