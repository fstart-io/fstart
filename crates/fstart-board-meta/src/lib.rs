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
use fstart_types::{hstr, Io16, IoAddr, PciBdf};

/// Convert runtime ACPI table descriptors into host-side board metadata.
///
/// This keeps board crates from hand-writing per-board adapter code between the
/// no_std table writer descriptors and the `fstart_types` metadata consumed by
/// xtask validation.
#[must_use]
pub fn acpi_config_from_platform(
    platform: &fstart_acpi::platform::PlatformConfig,
) -> fstart_types::acpi::AcpiConfig {
    match platform {
        fstart_acpi::platform::PlatformConfig::Arm(arm) => fstart_types::acpi::AcpiConfig {
            platform: fstart_types::acpi::AcpiPlatform::Arm(fstart_types::acpi::ArmPlatformAcpi {
                num_cpus: arm.num_cpus,
                gic_dist_base: arm.gic_dist_base,
                gic_redist_base: arm.gic_redist_base,
                gic_redist_length: arm.gic_redist_length,
                gic_its_base: arm.gic_its_base,
                timer_gsivs: arm.timer_gsivs,
                watchdog: arm
                    .watchdog
                    .as_ref()
                    .map(|watchdog| fstart_types::acpi::AcpiWatchdog {
                        refresh_base: watchdog.refresh_base,
                        control_base: watchdog.control_base,
                        gsiv: watchdog.gsiv,
                    }),
                iort: arm.iort.as_ref().map(|iort| fstart_types::acpi::AcpiIort {
                    its_ids: hvec_from_slice(iort.its_ids),
                    pci_segment: iort.pci_segment,
                    memory_address_limit: iort.memory_address_limit,
                    id_count: iort.id_count,
                }),
            }),
            print_hex: true,
        },
        #[allow(unreachable_patterns)]
        _ => fstart_types::acpi::AcpiConfig {
            platform: fstart_types::acpi::AcpiPlatform::X86,
            print_hex: true,
        },
    }
}

/// Convert a runtime AHCI ACPI descriptor into host ACPI-only metadata.
#[must_use]
pub fn ahci_extra_device(
    desc: &fstart_acpi::devices::AhciAcpi<'_>,
) -> fstart_types::acpi::AcpiExtraDevice {
    fstart_types::acpi::AcpiExtraDevice::Ahci(fstart_types::acpi::AcpiAhciDevice {
        name: hstr(desc.name),
        base: desc.base,
        size: desc.size,
        gsiv: desc.gsiv,
    })
}

/// Convert a runtime xHCI ACPI descriptor into host ACPI-only metadata.
#[must_use]
pub fn xhci_extra_device(
    desc: &fstart_acpi::devices::XhciAcpi<'_>,
) -> fstart_types::acpi::AcpiExtraDevice {
    fstart_types::acpi::AcpiExtraDevice::Xhci(fstart_types::acpi::AcpiXhciDevice {
        name: hstr(desc.name),
        base: desc.base,
        size: desc.size,
        gsiv: desc.gsiv,
    })
}

/// Convert a static SMBIOS descriptor into host-side board metadata.
#[must_use]
pub fn smbios_config_from_desc(desc: &fstart_smbios::SmbiosDesc<'_>) -> fstart_types::SmbiosConfig {
    let mut processors = heapless::Vec::new();
    for processor in desc.processors {
        let mut caches = heapless::Vec::new();
        for cache in processor.caches {
            caches
                .push(fstart_types::smbios::SmbiosCache {
                    designation: hstr(cache.designation),
                    level: cache.level,
                    size_kb: cache.size_kb,
                    associativity: cache_associativity_from_smbios(cache.associativity),
                    cache_type: cache_type_from_smbios(cache.cache_type),
                })
                .expect("SMBIOS cache metadata exceeds host capacity");
        }

        processors
            .push(fstart_types::smbios::SmbiosProcessor {
                socket: hstr(processor.socket),
                manufacturer: hstr(processor.manufacturer),
                processor_family: processor_family_from_smbios(processor.family),
                max_speed_mhz: Some(processor.max_speed_mhz),
                core_count: Some(processor.core_count),
                thread_count: Some(processor.thread_count),
                caches,
            })
            .expect("SMBIOS processor metadata exceeds host capacity");
    }

    let mut memory_devices = heapless::Vec::new();
    for memory in desc.memory_devices {
        memory_devices
            .push(fstart_types::smbios::SmbiosMemoryDevice {
                locator: hstr(memory.locator),
                size_mb: Some(memory.size_mb),
                speed_mhz: Some(memory.speed_mhz),
                memory_type: Some(memory_type_from_smbios(memory.memory_type)),
            })
            .expect("SMBIOS memory metadata exceeds host capacity");
    }

    fstart_types::SmbiosConfig {
        bios_vendor: hstr(desc.bios_vendor),
        bios_version: hstr(desc.bios_version),
        bios_release_date: hstr(desc.bios_release_date),
        system_manufacturer: hstr(desc.sys_manufacturer),
        system_product: hstr(desc.sys_product),
        system_version: hstr(desc.sys_version),
        system_serial: hstr(desc.sys_serial.unwrap_or("")),
        baseboard_manufacturer: hstr(desc.bb_manufacturer),
        baseboard_product: hstr(desc.bb_product),
        chassis_type: chassis_type_from_smbios(desc.chassis_type),
        chassis_manufacturer: hstr(desc.chassis_manufacturer),
        processors,
        memory_devices,
    }
}

fn hvec_from_slice<T: Copy, const N: usize>(items: &[T]) -> heapless::Vec<T, N> {
    let mut out = heapless::Vec::new();
    for item in items {
        out.push(*item)
            .ok()
            .expect("metadata exceeds host capacity");
    }
    out
}

fn chassis_type_from_smbios(value: u8) -> fstart_types::smbios::ChassisType {
    match value {
        0x03 => fstart_types::smbios::ChassisType::Desktop,
        0x04 => fstart_types::smbios::ChassisType::LowProfileDesktop,
        0x07 => fstart_types::smbios::ChassisType::Tower,
        0x17 => fstart_types::smbios::ChassisType::RackMount,
        0x1c => fstart_types::smbios::ChassisType::Blade,
        0x1d => fstart_types::smbios::ChassisType::Embedded,
        _ => fstart_types::smbios::ChassisType::Other,
    }
}

fn processor_family_from_smbios(value: u16) -> fstart_types::smbios::ProcessorFamily {
    match value {
        0x0118 => fstart_types::smbios::ProcessorFamily::Arm,
        0x0119 => fstart_types::smbios::ProcessorFamily::Aarch64,
        0x28 => fstart_types::smbios::ProcessorFamily::X86_64,
        0x0135 => fstart_types::smbios::ProcessorFamily::RiscV,
        _ => fstart_types::smbios::ProcessorFamily::Unknown,
    }
}

fn cache_associativity_from_smbios(value: u8) -> fstart_types::smbios::CacheAssociativity {
    match value {
        0x03 => fstart_types::smbios::CacheAssociativity::DirectMapped,
        0x04 => fstart_types::smbios::CacheAssociativity::Way2,
        0x05 => fstart_types::smbios::CacheAssociativity::Way4,
        0x06 => fstart_types::smbios::CacheAssociativity::FullyAssociative,
        0x07 => fstart_types::smbios::CacheAssociativity::Way8,
        0x09 => fstart_types::smbios::CacheAssociativity::Way16,
        _ => fstart_types::smbios::CacheAssociativity::Unknown,
    }
}

fn cache_type_from_smbios(value: u8) -> fstart_types::smbios::CacheType {
    match value {
        0x03 => fstart_types::smbios::CacheType::Instruction,
        0x04 => fstart_types::smbios::CacheType::Data,
        _ => fstart_types::smbios::CacheType::Unified,
    }
}

fn memory_type_from_smbios(value: u8) -> fstart_types::smbios::MemoryDeviceType {
    match value {
        0x13 => fstart_types::smbios::MemoryDeviceType::Ddr2,
        0x18 => fstart_types::smbios::MemoryDeviceType::Ddr3,
        0x1a => fstart_types::smbios::MemoryDeviceType::Ddr4,
        0x1b => fstart_types::smbios::MemoryDeviceType::Lpddr4,
        0x22 => fstart_types::smbios::MemoryDeviceType::Ddr5,
        0x23 => fstart_types::smbios::MemoryDeviceType::Lpddr5,
        _ => fstart_types::smbios::MemoryDeviceType::Unknown,
    }
}

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
    pub fn pci<D>(&mut self, name: &str, bdf: PciBdf, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.pci_enabled(name, bdf, true, driver)
    }

    /// Add a PCI child below this attachment point with an explicit enabled policy.
    pub fn pci_enabled<D>(&mut self, name: &str, bdf: PciBdf, enabled: bool, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::Pci(bdf.device, bdf.function),
            enabled,
            driver,
        )
    }

    /// Add an LPC child below this attachment point.
    pub fn lpc<D>(&mut self, name: &str, config_port: IoAddr<Io16>, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.lpc_enabled(name, config_port, true, driver)
    }

    /// Add an LPC child below this attachment point with an explicit enabled policy.
    pub fn lpc_enabled<D>(
        &mut self,
        name: &str,
        config_port: IoAddr<Io16>,
        enabled: bool,
        driver: D,
    ) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::Lpc(config_port.raw()),
            enabled,
            driver,
        )
    }

    /// Add an SMBus/I2C-addressed child below this attachment point.
    pub fn i2c<D>(&mut self, name: &str, address: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.i2c_enabled(name, address, true, driver)
    }

    /// Add an SMBus-addressed child below this attachment point.
    pub fn smbus<D>(&mut self, name: &str, address: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.i2c(name, address, driver)
    }

    /// Add an SMBus-addressed child below this attachment point with an explicit enabled policy.
    pub fn smbus_enabled<D>(
        &mut self,
        name: &str,
        address: u8,
        enabled: bool,
        driver: D,
    ) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.i2c_enabled(name, address, enabled, driver)
    }

    /// Add an I2C-addressed child below this attachment point with an explicit enabled policy.
    pub fn i2c_enabled<D>(&mut self, name: &str, address: u8, enabled: bool, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::I2c(address),
            enabled,
            driver,
        )
    }

    /// Add an SPI child below this attachment point.
    pub fn spi<D>(&mut self, name: &str, chip_select: u8, driver: D) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.spi_enabled(name, chip_select, true, driver)
    }

    /// Add an SPI child below this attachment point with an explicit enabled policy.
    pub fn spi_enabled<D>(
        &mut self,
        name: &str,
        chip_select: u8,
        enabled: bool,
        driver: D,
    ) -> &mut Self
    where
        D: BoardDriver + Clone,
    {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::Spi(chip_select),
            enabled,
            driver,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_services::ServiceSet;
    use fstart_types::{io16, BusAddress, DeviceRole, PciBdf};

    #[derive(Debug, Clone)]
    struct TestDriver(&'static str);

    impl BoardDriver for TestDriver {
        fn feature(&self) -> &'static str {
            self.0
        }

        fn services(&self) -> ServiceSet {
            ServiceSet::empty()
        }

        fn clone_box(&self) -> Box<dyn BoardDriver> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn attach_point_authors_typed_bus_children_with_enabled_policy() {
        let mut extensions = PlatformDeviceExtensions::new();
        extensions.on("southbridge", |bus| {
            bus.pci_enabled("ethernet", PciBdf::new(0, 3, 0), false, TestDriver("e1000"))
                .lpc("superio", io16(0x2e), TestDriver("superio"))
                .smbus_enabled("spd0", 0x50, true, TestDriver("spd"))
                .spi_enabled("flash0", 0, false, TestDriver("spi-flash"));
        });

        let (devices, bindings) = PlatformTopology::new()
            .root("southbridge", TestDriver("ich"))
            .extend(&extensions)
            .build();

        assert_eq!(devices.len(), 5);
        assert_eq!(bindings.len(), 5);

        let ethernet = devices.iter().find(|d| d.name == "ethernet").unwrap();
        assert_eq!(ethernet.parent.as_deref(), Some("southbridge"));
        assert_eq!(ethernet.bus, Some(BusAddress::Pci(3, 0)));
        assert_eq!(ethernet.role, DeviceRole::Runtime);
        assert!(!ethernet.enabled);

        let superio = devices.iter().find(|d| d.name == "superio").unwrap();
        assert_eq!(superio.bus, Some(BusAddress::Lpc(0x2e)));
        assert!(superio.enabled);

        let spd = devices.iter().find(|d| d.name == "spd0").unwrap();
        assert_eq!(spd.bus, Some(BusAddress::I2c(0x50)));

        let flash = devices.iter().find(|d| d.name == "flash0").unwrap();
        assert_eq!(flash.bus, Some(BusAddress::Spi(0)));
        assert!(!flash.enabled);

        assert!(bindings.iter().any(|binding| binding.device == "ethernet"));
        assert!(bindings.iter().any(|binding| binding.device == "flash0"));
    }

    #[test]
    fn platform_topology_builds_structural_bus_nodes() {
        let devices = PlatformTopology::new()
            .root("host", TestDriver("host"))
            .pci_bridge("host", "pcie-root-port0", 1, 0, true)
            .child_bus("host", "lpc", DeviceRole::LpcBus)
            .build_devices();

        let port = devices
            .iter()
            .find(|device| device.name == "pcie-root-port0")
            .unwrap();
        assert_eq!(port.parent.as_deref(), Some("host"));
        assert_eq!(port.bus, Some(BusAddress::Pci(1, 0)));
        assert_eq!(port.role, DeviceRole::PciBridge);

        let lpc = devices.iter().find(|device| device.name == "lpc").unwrap();
        assert_eq!(lpc.parent.as_deref(), Some("host"));
        assert_eq!(lpc.bus, None);
        assert_eq!(lpc.role, DeviceRole::LpcBus);
    }
}
