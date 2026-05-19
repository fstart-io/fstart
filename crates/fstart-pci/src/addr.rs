//! PCI address types.

/// PCI bus/device/function address local to one segment/root bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciBdf {
    /// Bus number.
    pub bus: u8,
    /// Device number on the bus.
    pub dev: u8,
    /// Function number within the device.
    pub func: u8,
}

impl PciBdf {
    /// Create a segment-local PCI BDF address.
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self {
        Self { bus, dev, func }
    }
}

/// Full PCI segment:bus/device/function address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciSbdf {
    /// PCI segment/domain number.
    pub segment: u16,
    /// Segment-local bus/device/function address.
    pub bdf: PciBdf,
}

impl PciSbdf {
    /// Create a full PCI address.
    pub const fn new(segment: u16, bus: u8, dev: u8, func: u8) -> Self {
        Self {
            segment,
            bdf: PciBdf::new(bus, dev, func),
        }
    }
}
