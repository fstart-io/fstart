//! Board-owned topology and table metadata helpers.
//!
//! This crate is the neutral replacement for the old device registry's common
//! metadata pieces. It intentionally has no dependency on concrete runtime driver
//! crates: boards choose runtime drivers in their stage/facts code, while host
//! metadata describes firmware layout, tables, and device topology only.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unexpected_cfgs)]
extern crate alloc;

use alloc::vec::Vec;
use heapless::String as HString;
use serde::{Deserialize, Serialize};

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

/// Platform-owned topology template with optional board-authored runtime devices.
#[derive(Debug, Clone)]
pub struct PlatformTopology {
    topology: fstart_types::DeviceTopology,
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
    pub fn runtime(&mut self, name: &str, bus: fstart_types::BusAddress) -> &mut Self {
        self.runtime_enabled(name, bus, true)
    }

    /// Add a runtime child with an explicit enabled policy.
    pub fn runtime_enabled(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
    ) -> &mut Self {
        self.extensions.runtime_devices.push(PlatformRuntimeDevice {
            name: fstart_types::hstr(name),
            parent: fstart_types::hstr(self.parent),
            bus,
            enabled,
        });
        self
    }

    /// Add a PCI child below this attachment point.
    pub fn pci(&mut self, name: &str, bdf: PciBdf) -> &mut Self {
        self.pci_enabled(name, bdf, true)
    }

    /// Add a PCI child below this attachment point with an explicit enabled policy.
    pub fn pci_enabled(&mut self, name: &str, bdf: PciBdf, enabled: bool) -> &mut Self {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::Pci(bdf.device, bdf.function),
            enabled,
        )
    }

    /// Add an LPC child below this attachment point.
    pub fn lpc(&mut self, name: &str, config_port: IoAddr<Io16>) -> &mut Self {
        self.lpc_enabled(name, config_port, true)
    }

    /// Add an LPC child below this attachment point with an explicit enabled policy.
    pub fn lpc_enabled(
        &mut self,
        name: &str,
        config_port: IoAddr<Io16>,
        enabled: bool,
    ) -> &mut Self {
        self.runtime_enabled(
            name,
            fstart_types::BusAddress::Lpc(config_port.raw()),
            enabled,
        )
    }

    /// Add an SMBus/I2C-addressed child below this attachment point.
    pub fn i2c(&mut self, name: &str, address: u8) -> &mut Self {
        self.i2c_enabled(name, address, true)
    }

    /// Add an SMBus-addressed child below this attachment point.
    pub fn smbus(&mut self, name: &str, address: u8) -> &mut Self {
        self.i2c(name, address)
    }

    /// Add an SMBus-addressed child below this attachment point with an explicit enabled policy.
    pub fn smbus_enabled(&mut self, name: &str, address: u8, enabled: bool) -> &mut Self {
        self.i2c_enabled(name, address, enabled)
    }

    /// Add an I2C-addressed child below this attachment point with an explicit enabled policy.
    pub fn i2c_enabled(&mut self, name: &str, address: u8, enabled: bool) -> &mut Self {
        self.runtime_enabled(name, fstart_types::BusAddress::I2c(address), enabled)
    }

    /// Add an SPI child below this attachment point.
    pub fn spi(&mut self, name: &str, chip_select: u8) -> &mut Self {
        self.spi_enabled(name, chip_select, true)
    }

    /// Add an SPI child below this attachment point with an explicit enabled policy.
    pub fn spi_enabled(&mut self, name: &str, chip_select: u8, enabled: bool) -> &mut Self {
        self.runtime_enabled(name, fstart_types::BusAddress::Spi(chip_select), enabled)
    }
}

impl PlatformTopology {
    /// Start an empty platform topology template.
    #[must_use]
    pub fn new() -> Self {
        Self {
            topology: fstart_types::DeviceTopology::new(),
        }
    }

    /// Add a root runtime device.
    #[must_use]
    pub fn root(mut self, name: &str) -> Self {
        self.topology = self.topology.root(name);
        self
    }

    /// Add a root runtime device with an explicit enabled policy.
    #[must_use]
    pub fn root_enabled(mut self, name: &str, enabled: bool) -> Self {
        self.topology = self.topology.runtime_root(name, enabled);
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

    /// Add a runtime child.
    #[must_use]
    pub fn runtime(
        mut self,
        parent: &str,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
    ) -> Self {
        self.topology = self.topology.runtime_child(parent, name, bus, enabled);
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
        }
        self
    }

    /// Finish as the flattened runtime device table.
    #[must_use]
    pub fn build_devices(self) -> heapless::Vec<fstart_types::DeviceConfig, 32> {
        self.topology.build()
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
    use fstart_types::{io16, BusAddress, DeviceRole};

    #[test]
    fn attach_point_authors_typed_bus_children_with_enabled_policy() {
        let mut extensions = PlatformDeviceExtensions::new();
        extensions.on("southbridge", |bus| {
            bus.pci_enabled("ethernet", PciBdf::new(0, 3, 0), false)
                .lpc("superio", io16(0x2e))
                .smbus_enabled("spd0", 0x50, true)
                .spi_enabled("flash0", 0, false);
        });

        let devices = PlatformTopology::new()
            .root("southbridge")
            .extend(&extensions)
            .build_devices();

        assert_eq!(devices.len(), 5);

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
    }

    #[test]
    fn platform_topology_builds_structural_bus_nodes() {
        let devices = PlatformTopology::new()
            .root("host")
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
