//! Strongly typed resource and topology values for Rust board builders.
//!
//! These types are intentionally small, serializable facts that can be used by
//! both static Rust board crates and the future dynamic board-blob ABI. Driver
//! crates can add their own marker types to distinguish register widths, I/O
//! spaces, and bus ports at compile time.

use core::marker::PhantomData;

use heapless::String as HString;
use serde::{Deserialize, Serialize};

use crate::DeviceId;

/// Marker for 8-bit MMIO register access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmio8 {}

/// Marker for 16-bit MMIO register access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmio16 {}

/// Marker for 32-bit MMIO register access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmio32 {}

/// Marker for 64-bit MMIO register access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmio64 {}

/// Marker for 8-bit port-I/O access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io8 {}

/// Marker for 16-bit port-I/O access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io16 {}

/// Typed memory-mapped I/O address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MmioAddr<T> {
    raw: u64,
    #[serde(skip)]
    _kind: PhantomData<T>,
}

impl<T> MmioAddr<T> {
    /// Construct a typed MMIO address from its raw physical address.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self {
            raw,
            _kind: PhantomData,
        }
    }

    /// Return the raw physical address.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

/// Typed port-I/O address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IoAddr<T> {
    raw: u16,
    #[serde(skip)]
    _kind: PhantomData<T>,
}

impl<T> IoAddr<T> {
    /// Construct a typed port-I/O address from its raw port value.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self {
            raw,
            _kind: PhantomData,
        }
    }

    /// Return the raw port value.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.raw
    }
}

/// Construct a 32-bit MMIO address.
#[must_use]
pub const fn mmio32(raw: u64) -> MmioAddr<Mmio32> {
    MmioAddr::new(raw)
}

/// Construct a 16-bit port-I/O address.
#[must_use]
pub const fn io16(raw: u16) -> IoAddr<Io16> {
    IoAddr::new(raw)
}

/// Interrupt request line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Irq(pub u8);

/// PCI bus/device/function address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PciBdf {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub device: u8,
    /// PCI function number.
    pub function: u8,
}

impl PciBdf {
    /// Construct a PCI BDF address.
    #[must_use]
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }
}

/// Generic bus kind used by lowered topology and dynamic board blobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusKind {
    /// PCI or PCI Express bus.
    Pci,
    /// LPC/ISA-style bus.
    Lpc,
    /// I2C bus.
    I2c,
    /// SMBus.
    Smbus,
    /// SPI bus.
    Spi,
    /// Non-enumerable memory-mapped simple bus.
    SimpleBus,
}

/// Stable parent-local bus port identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusPortId {
    /// Human-readable port name, stable within the parent device.
    pub name: HString<32>,
    /// Kind of bus exposed by this port.
    pub kind: BusKind,
}

impl BusPortId {
    /// Construct a bus port identifier.
    #[must_use]
    pub fn new(name: &str, kind: BusKind) -> Self {
        let mut port_name = HString::new();
        port_name
            .push_str(name)
            .expect("bus port name exceeds BusPortId capacity");
        Self {
            name: port_name,
            kind,
        }
    }
}

/// Marker for a typed PCI child bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBus {}

/// Marker for a typed LPC child bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpcBus {}

/// Marker for a typed I2C child bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cBus {}

/// Marker for a typed SMBus child bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmbusBus {}

/// Marker for a typed SPI child bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiBus {}

/// Marker for a typed non-enumerable simple bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleBus {}

/// Trait implemented by typed bus markers.
pub trait TypedBus {
    /// Lowered generic bus kind.
    const KIND: BusKind;
}

impl TypedBus for PciBus {
    const KIND: BusKind = BusKind::Pci;
}

impl TypedBus for LpcBus {
    const KIND: BusKind = BusKind::Lpc;
}

impl TypedBus for I2cBus {
    const KIND: BusKind = BusKind::I2c;
}

impl TypedBus for SmbusBus {
    const KIND: BusKind = BusKind::Smbus;
}

impl TypedBus for SpiBus {
    const KIND: BusKind = BusKind::Spi;
}

impl TypedBus for SimpleBus {
    const KIND: BusKind = BusKind::SimpleBus;
}

/// A typed child attachment to a parent-local bus port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildAttachment<B> {
    /// Parent device ID.
    pub parent: DeviceId,
    /// Child device ID.
    pub child: DeviceId,
    /// Parent-local port name.
    pub port_name: HString<32>,
    /// Optional lowered bus address.
    pub address: Option<crate::BusAddress>,
    _bus: PhantomData<B>,
}

impl<B: TypedBus> ChildAttachment<B> {
    /// Lower this typed attachment to the generic topology edge.
    #[must_use]
    pub fn lower(self) -> DeviceEdge {
        DeviceEdge {
            parent: self.parent,
            child: self.child,
            port: BusPortId {
                name: self.port_name,
                kind: B::KIND,
            },
            address: self.address,
        }
    }
}

/// Attach a child on a typed PCI port using a BDF address.
#[must_use]
pub fn pci_child(
    parent: DeviceId,
    child: DeviceId,
    port_name: &str,
    bdf: PciBdf,
) -> ChildAttachment<PciBus> {
    let mut name = HString::new();
    name.push_str(port_name)
        .expect("bus port name exceeds ChildAttachment capacity");
    ChildAttachment {
        parent,
        child,
        port_name: name,
        address: Some(crate::BusAddress::Pci(bdf.device, bdf.function)),
        _bus: PhantomData,
    }
}

/// Attach a child on a typed LPC port using a config-port address.
#[must_use]
pub fn lpc_child(
    parent: DeviceId,
    child: DeviceId,
    port_name: &str,
    config_port: IoAddr<Io16>,
) -> ChildAttachment<LpcBus> {
    let mut name = HString::new();
    name.push_str(port_name)
        .expect("bus port name exceeds ChildAttachment capacity");
    ChildAttachment {
        parent,
        child,
        port_name: name,
        address: Some(crate::BusAddress::Lpc(config_port.raw())),
        _bus: PhantomData,
    }
}

/// Attach a child on a typed I2C port using a 7-bit address.
#[must_use]
pub fn i2c_child(
    parent: DeviceId,
    child: DeviceId,
    port_name: &str,
    address: u8,
) -> ChildAttachment<I2cBus> {
    let mut name = HString::new();
    name.push_str(port_name)
        .expect("bus port name exceeds ChildAttachment capacity");
    ChildAttachment {
        parent,
        child,
        port_name: name,
        address: Some(crate::BusAddress::I2c(address)),
        _bus: PhantomData,
    }
}

/// Attach a child on a typed SPI port using a chip-select index.
#[must_use]
pub fn spi_child(
    parent: DeviceId,
    child: DeviceId,
    port_name: &str,
    chip_select: u8,
) -> ChildAttachment<SpiBus> {
    let mut name = HString::new();
    name.push_str(port_name)
        .expect("bus port name exceeds ChildAttachment capacity");
    ChildAttachment {
        parent,
        child,
        port_name: name,
        address: Some(crate::BusAddress::Spi(chip_select)),
        _bus: PhantomData,
    }
}

/// Lowered topology edge between two devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceEdge {
    /// Parent device ID.
    pub parent: DeviceId,
    /// Child device ID.
    pub child: DeviceId,
    /// Parent-local bus port used by the child.
    pub port: BusPortId,
    /// Optional address of the child on the bus.
    pub address: Option<crate::BusAddress>,
}
