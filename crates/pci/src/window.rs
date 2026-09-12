//! PCI resource window types.

/// Kind of address space a [`PciWindow`] decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PciWindowKind {
    /// Memory-mapped I/O.
    Mmio,
    /// I/O port space, possibly memory-mapped by the host bridge.
    Io,
}

/// One address window decoded by a PCI root bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciWindow {
    /// Kind of address space.
    pub kind: PciWindowKind,
    /// Base physical address or I/O port address.
    pub base: u64,
    /// Size of the window in bytes.
    pub size: u64,
    /// Whether this memory window supports prefetchable transactions.
    pub prefetchable: bool,
}

impl PciWindow {
    /// Exclusive end address (`base + size`), saturating at `u64::MAX`.
    pub const fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    /// Whether a 32-bit BAR can target this window.
    pub const fn is_below_4g(&self) -> bool {
        self.base.saturating_add(self.size) <= 0x1_0000_0000
    }
}
