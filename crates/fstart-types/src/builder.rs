//! Plain Rust board and build metadata builders.
//!
//! These builders are the typed Rust replacement path for parser-specific board
//! authoring. They intentionally describe facts only: board config helpers and
//! typed topology tables. The fixed stage runner owns runtime control flow.

use heapless::{String as HString, Vec as HVec};

use crate::{
    BusAddress, Compression, DeviceConfig, DeviceEdge, DeviceRole, DigestAlgorithm, FdtSource,
    I2cBus, Io16, IoAddr, LpcBus, PayloadConfig, PayloadKind, PciBdf, PciBus, PnpBus,
    SecurityConfig, SignatureAlgorithm, SmbusBus, SpiBus, TypedBus,
};

/// Construct a bounded heapless string for static board metadata.
///
/// This keeps board crates from each defining their own `HString::try_from`
/// wrappers while still failing fast when a literal exceeds the schema capacity.
#[must_use]
pub fn hstr<const N: usize>(value: &str) -> HString<N> {
    HString::try_from(value).expect("string exceeds heapless capacity")
}

/// Construct a bounded heapless vector for static board metadata.
///
/// The output capacity `C` is inferred from the destination field type.
#[must_use]
pub fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}

/// Default x86 LinuxBoot payload policy used by simple PC-compatible boards.
#[must_use]
pub fn x86_linuxboot_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("bzImage")),
        kernel_load_addr: Some(0x0200_0000),
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: Some(hstr(
            "console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 ignore_loglevel loglevel=8",
        )),
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Default x86 UEFI payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_uefi_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::UefiPayload,
        kernel_file: None,
        kernel_load_addr: None,
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Generic board-device topology builder.
///
/// Platform and board crates should use this for flat [`DeviceConfig`] tables
/// instead of each carrying local `push`/capacity boilerplate. It deliberately
/// records topology facts only. Runtime driver bindings live separately and are
/// matched by device name by host codegen.
#[derive(Debug, Clone)]
pub struct DeviceTopology {
    devices: HVec<DeviceConfig, 32>,
    edges: HVec<DeviceEdge, 64>,
}

impl DeviceTopology {
    /// Start an empty device topology.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            devices: HVec::new(),
            edges: HVec::new(),
        }
    }

    /// Add a root runtime device.
    #[must_use]
    pub fn root(self, name: &str) -> Self {
        self.device(name, None, None, DeviceRole::Runtime, true)
    }

    /// Add a root runtime device with an explicit enabled policy.
    #[must_use]
    pub fn runtime_root(self, name: &str, enabled: bool) -> Self {
        self.device(name, None, None, DeviceRole::Runtime, enabled)
    }

    /// Add a child runtime device.
    #[must_use]
    pub fn child(self, parent: &str, name: &str, bus: BusAddress) -> Self {
        self.device(name, Some(parent), Some(bus), DeviceRole::Runtime, true)
    }

    /// Add a child runtime device with an explicit enabled policy.
    #[must_use]
    pub fn runtime_child(self, parent: &str, name: &str, bus: BusAddress, enabled: bool) -> Self {
        self.device(name, Some(parent), Some(bus), DeviceRole::Runtime, enabled)
    }

    /// Add a driverless child bus owned by its parent device.
    #[must_use]
    pub fn child_bus(self, parent: &str, name: &str, role: DeviceRole) -> Self {
        assert!(!role.is_runtime(), "child_bus requires a structural role");
        self.device(name, Some(parent), None, role, true)
    }

    /// Add a driverless child bus and declare its children in a scoped branch.
    #[must_use]
    pub fn bus<'a, F>(self, parent: &str, name: &'a str, role: DeviceRole, children: F) -> Self
    where
        F: FnOnce(DeviceBranch<'a>) -> DeviceBranch<'a>,
    {
        let topology = self.child_bus(parent, name, role);
        children(DeviceBranch {
            topology,
            parent: name,
            _bus: core::marker::PhantomData,
        })
        .finish()
    }

    /// Add a typed child bus and declare its children in a scoped branch.
    #[must_use]
    pub fn typed_bus<'a, B, F>(
        self,
        parent: &str,
        name: &'a str,
        role: DeviceRole,
        children: F,
    ) -> Self
    where
        B: TypedBus,
        F: FnOnce(DeviceBranch<'a, B>) -> DeviceBranch<'a, B>,
    {
        let topology = self.child_bus(parent, name, role);
        children(DeviceBranch {
            topology,
            parent: name,
            _bus: core::marker::PhantomData,
        })
        .finish()
    }

    /// Add a typed PCI child bus.
    #[must_use]
    pub fn pci_bus<'a, F>(self, parent: &str, name: &'a str, children: F) -> Self
    where
        F: FnOnce(DeviceBranch<'a, PciBus>) -> DeviceBranch<'a, PciBus>,
    {
        self.typed_bus::<PciBus, _>(parent, name, DeviceRole::PciBridge, children)
    }

    /// Add a typed LPC child bus.
    #[must_use]
    pub fn lpc_bus<'a, F>(self, parent: &str, name: &'a str, children: F) -> Self
    where
        F: FnOnce(DeviceBranch<'a, LpcBus>) -> DeviceBranch<'a, LpcBus>,
    {
        self.typed_bus::<LpcBus, _>(parent, name, DeviceRole::LpcBus, children)
    }

    /// Add a typed SMBus child bus.
    #[must_use]
    pub fn smbus<'a, F>(self, parent: &str, name: &'a str, children: F) -> Self
    where
        F: FnOnce(DeviceBranch<'a, SmbusBus>) -> DeviceBranch<'a, SmbusBus>,
    {
        self.typed_bus::<SmbusBus, _>(parent, name, DeviceRole::SmBus, children)
    }

    /// Add a driverless PCI/PCIe bridge/root-port node.
    #[must_use]
    pub fn pci_bridge(
        self,
        parent: &str,
        name: &str,
        device: u8,
        function: u8,
        enabled: bool,
    ) -> Self {
        self.device(
            name,
            Some(parent),
            Some(BusAddress::Pci(device, function)),
            DeviceRole::PciBridge,
            enabled,
        )
    }

    /// Finish as the bounded flat table consumed by existing board metadata.
    #[must_use]
    pub fn build(self) -> HVec<DeviceConfig, 32> {
        self.devices
    }

    /// Finish as both the compatibility flat table and the lowered typed edges.
    #[must_use]
    pub fn build_graph(self) -> (HVec<DeviceConfig, 32>, HVec<DeviceEdge, 64>) {
        (self.devices, self.edges)
    }

    fn device(
        mut self,
        name: &str,
        parent: Option<&str>,
        bus: Option<BusAddress>,
        role: DeviceRole,
        enabled: bool,
    ) -> Self {
        let parent_id = parent.and_then(|parent| {
            self.devices
                .iter()
                .position(|candidate| candidate.name.as_str() == parent)
                .map(|idx| idx as crate::DeviceId)
        });
        let child_id = self.devices.len() as crate::DeviceId;

        self.devices
            .push(DeviceConfig {
                name: hstr(name),
                parent: parent.map(hstr),
                bus,
                role,
                enabled,
            })
            .expect("device table capacity");
        if let Some(parent_id) = parent_id {
            self.edges
                .push(DeviceEdge {
                    parent: parent_id,
                    child: child_id,
                    port: crate::BusPortId::new(parent.unwrap_or("root"), bus_kind(role, bus)),
                    address: bus,
                })
                .expect("device edge capacity");
        }
        self
    }
}

fn bus_kind(role: DeviceRole, bus: Option<BusAddress>) -> crate::BusKind {
    match bus {
        Some(BusAddress::Pci(_, _)) => crate::BusKind::Pci,
        Some(BusAddress::Lpc(_)) => crate::BusKind::Lpc,
        Some(BusAddress::I2c(_)) => crate::BusKind::I2c,
        Some(BusAddress::Spi(_)) => crate::BusKind::Spi,
        Some(BusAddress::Pnp(_)) => crate::BusKind::Pnp,
        None => match role {
            DeviceRole::PciBridge => crate::BusKind::Pci,
            DeviceRole::LpcBus => crate::BusKind::Lpc,
            DeviceRole::SmBus => crate::BusKind::Smbus,
            DeviceRole::PnpDevice => crate::BusKind::Pnp,
            DeviceRole::GenericBus | DeviceRole::Runtime => crate::BusKind::SimpleBus,
        },
    }
}

/// Scoped child builder returned by [`DeviceTopology::bus`].
#[derive(Debug, Clone)]
pub struct DeviceBranch<'a, B = crate::SimpleBus> {
    topology: DeviceTopology,
    parent: &'a str,
    _bus: core::marker::PhantomData<B>,
}

/// Lowered child device produced by typed bus-child descriptors.
#[derive(Debug, Clone)]
pub struct TopologyChild {
    name: HString<32>,
    address: BusAddress,
    enabled: bool,
}

impl TopologyChild {
    /// Construct a lowered child device attachment.
    pub fn new(name: &str, address: BusAddress) -> Self {
        Self {
            name: hstr(name),
            address,
            enabled: true,
        }
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A child descriptor that can attach to bus `B`.
pub trait BusChild<B: TypedBus> {
    /// Lower this typed child into generic topology facts.
    fn into_topology_child(self) -> TopologyChild;
}

/// Child device that attaches to a PCI bus.
#[derive(Debug, Clone)]
pub struct PciChild(TopologyChild);

impl PciChild {
    /// Create a PCI child at the given BDF.
    pub fn new(name: &str, bdf: PciBdf) -> Self {
        Self(TopologyChild::new(
            name,
            BusAddress::Pci(bdf.device, bdf.function),
        ))
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }
}

impl BusChild<PciBus> for PciChild {
    fn into_topology_child(self) -> TopologyChild {
        self.0
    }
}

/// Child device that attaches to an LPC bus.
#[derive(Debug, Clone)]
pub struct LpcChild(TopologyChild);

impl LpcChild {
    /// Create an LPC child at the given config-port address.
    pub fn new(name: &str, config_port: IoAddr<Io16>) -> Self {
        Self(TopologyChild::new(name, BusAddress::Lpc(config_port.raw())))
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }
}

impl BusChild<LpcBus> for LpcChild {
    fn into_topology_child(self) -> TopologyChild {
        self.0
    }
}

/// Child device that attaches to an I2C bus.
#[derive(Debug, Clone)]
pub struct I2cChild(TopologyChild);

impl I2cChild {
    /// Create an I2C child at the given 7-bit address.
    pub fn new(name: &str, address: u8) -> Self {
        Self(TopologyChild::new(name, BusAddress::I2c(address)))
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }
}

impl BusChild<I2cBus> for I2cChild {
    fn into_topology_child(self) -> TopologyChild {
        self.0
    }
}

/// Child device that attaches to an SMBus.
#[derive(Debug, Clone)]
pub struct SmbusChild(TopologyChild);

impl SmbusChild {
    /// Create an SMBus child at the given 7-bit address.
    pub fn new(name: &str, address: u8) -> Self {
        Self(TopologyChild::new(name, BusAddress::I2c(address)))
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }
}

impl BusChild<SmbusBus> for SmbusChild {
    fn into_topology_child(self) -> TopologyChild {
        self.0
    }
}

/// Child device that attaches to an SPI bus.
#[derive(Debug, Clone)]
pub struct SpiChild(TopologyChild);

impl SpiChild {
    /// Create an SPI child at the given chip-select index.
    pub fn new(name: &str, chip_select: u8) -> Self {
        Self(TopologyChild::new(name, BusAddress::Spi(chip_select)))
    }

    /// Set whether the child is present/enabled in this board configuration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.0 = self.0.enabled(enabled);
        self
    }
}

impl BusChild<SpiBus> for SpiChild {
    fn into_topology_child(self) -> TopologyChild {
        self.0
    }
}

impl<'a, B> DeviceBranch<'a, B> {
    /// Attach a typed child accepted by this branch's bus type.
    pub fn attach<C>(self, child: C) -> Self
    where
        B: TypedBus,
        C: BusChild<B>,
    {
        let child = child.into_topology_child();
        Self {
            topology: self.topology.runtime_child(
                self.parent,
                child.name.as_str(),
                child.address,
                child.enabled,
            ),
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }

    /// Add a runtime child to this branch's parent bus.
    #[must_use]
    pub fn child(self, name: &str, bus: BusAddress) -> Self {
        Self {
            topology: self.topology.child(self.parent, name, bus),
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }

    /// Add a runtime child to this branch's parent bus with an explicit enabled policy.
    #[must_use]
    pub fn runtime_child(self, name: &str, bus: BusAddress, enabled: bool) -> Self {
        Self {
            topology: self.topology.runtime_child(self.parent, name, bus, enabled),
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }

    /// Add a nested driverless bus below this branch.
    #[must_use]
    pub fn bus<'b, F>(self, name: &'b str, role: DeviceRole, children: F) -> DeviceBranch<'a>
    where
        F: FnOnce(DeviceBranch<'b>) -> DeviceBranch<'b>,
    {
        let topology = self.topology.bus(self.parent, name, role, children);
        DeviceBranch {
            topology,
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }

    fn finish(self) -> DeviceTopology {
        self.topology
    }
}

impl<'a> DeviceBranch<'a, PciBus> {
    /// Add a child on this PCI bus using a typed BDF address.
    #[must_use]
    pub fn pci_device(self, name: &str, bdf: PciBdf) -> Self {
        self.attach(PciChild::new(name, bdf))
    }
}

impl<'a> DeviceBranch<'a, LpcBus> {
    /// Add a child on this LPC bus using a typed config-port address.
    #[must_use]
    pub fn lpc_device(self, name: &str, config_port: IoAddr<Io16>) -> Self {
        self.attach(LpcChild::new(name, config_port))
    }

    /// Add a SuperIO chip on this LPC bus and describe its PnP logical devices.
    pub fn superio<'b, F>(
        self,
        name: &'b str,
        config_port: IoAddr<Io16>,
        enabled: bool,
        ldns: F,
    ) -> Self
    where
        F: FnOnce(DeviceBranch<'b, PnpBus>) -> DeviceBranch<'b, PnpBus>,
    {
        let topology = self.topology.device(
            name,
            Some(self.parent),
            Some(BusAddress::Lpc(config_port.raw())),
            DeviceRole::Runtime,
            enabled,
        );
        let topology = ldns(DeviceBranch {
            topology,
            parent: name,
            _bus: core::marker::PhantomData,
        })
        .finish();
        DeviceBranch {
            topology,
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }
}

impl<'a> DeviceBranch<'a, PnpBus> {
    /// Add a structural Plug-and-Play logical device below a SuperIO chip.
    pub fn ldn(self, name: &str, ldn: u8, enabled: bool) -> Self {
        Self {
            topology: self.topology.device(
                name,
                Some(self.parent),
                Some(BusAddress::Pnp(ldn)),
                DeviceRole::PnpDevice,
                enabled,
            ),
            parent: self.parent,
            _bus: core::marker::PhantomData,
        }
    }
}

impl<'a> DeviceBranch<'a, SmbusBus> {
    /// Add a child on this SMBus using a 7-bit address.
    #[must_use]
    pub fn smbus_device(self, name: &str, address: u8) -> Self {
        self.attach(SmbusChild::new(name, address))
    }
}

impl<'a> DeviceBranch<'a, I2cBus> {
    /// Add a child on this I2C bus using a 7-bit address.
    #[must_use]
    pub fn i2c_device(self, name: &str, address: u8) -> Self {
        self.attach(I2cChild::new(name, address))
    }
}

impl<'a> DeviceBranch<'a, SpiBus> {
    /// Add a child on this SPI bus using a chip-select index.
    #[must_use]
    pub fn spi_device(self, name: &str, chip_select: u8) -> Self {
        self.attach(SpiChild::new(name, chip_select))
    }
}

impl Default for DeviceTopology {
    fn default() -> Self {
        Self::new()
    }
}

/// Common development signing policy for board metadata.
#[must_use]
pub fn dev_security_config(pubkey_file: &str) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr(pubkey_file),
        required_digests: hvec([DigestAlgorithm::Sha256]),
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceTopology;
    use crate::{io16, BusKind, BusPortId};

    #[test]
    #[should_panic(expected = "bus port name exceeds BusPortId capacity")]
    fn bus_port_id_rejects_over_capacity_names() {
        let name = "x".repeat(33);
        let _ = BusPortId::new(&name, BusKind::Lpc);
    }

    #[test]
    fn device_topology_lowers_typed_bus_to_flat_table_and_edges() {
        let topology = DeviceTopology::new()
            .root("southbridge")
            .lpc_bus("southbridge", "lpc", |lpc| {
                lpc.superio("superio", io16(0x2e), true, |pnp| {
                    pnp.ldn("superio_com1", 0x01, true)
                })
            })
            .smbus("southbridge", "smbus", |smbus| {
                smbus.smbus_device("spd0", 0x50)
            });
        let (devices, edges) = topology.build_graph();

        assert_eq!(devices.len(), 6);
        assert_eq!(devices[2].parent.as_ref().unwrap().as_str(), "lpc");
        assert_eq!(devices[2].bus, Some(crate::BusAddress::Lpc(0x2e)));
        assert_eq!(devices[3].parent.as_ref().unwrap().as_str(), "superio");
        assert_eq!(devices[3].bus, Some(crate::BusAddress::Pnp(0x01)));
        assert_eq!(devices[5].parent.as_ref().unwrap().as_str(), "smbus");
        assert_eq!(devices[5].bus, Some(crate::BusAddress::I2c(0x50)));
        assert_eq!(edges.len(), 5);
        assert_eq!(edges[1].parent, 1);
        assert_eq!(edges[1].child, 2);
        assert_eq!(edges[1].port.kind, BusKind::Lpc);
        assert_eq!(edges[2].parent, 2);
        assert_eq!(edges[2].child, 3);
        assert_eq!(edges[2].port.kind, BusKind::Pnp);
    }
}
