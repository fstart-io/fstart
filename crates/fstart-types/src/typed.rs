//! Strongly typed resource values for Rust board config.

use core::marker::PhantomData;

use serde::{Deserialize, Serialize};

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
