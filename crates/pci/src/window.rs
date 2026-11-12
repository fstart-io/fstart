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

/// One span of PCI address space: a window of a root bridge, a part of one
/// the firmware reserved, or the range a BAR was assigned.
///
/// Whether a BAR is prefetchable is the BAR's own property, and where the
/// allocator puts it its own policy (below 4 GiB first, above only what
/// may go there); a window does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciWindow {
    /// Kind of address space.
    pub kind: PciWindowKind,
    /// Base physical address or I/O port address.
    pub base: u64,
    /// Size of the window in bytes.
    pub size: u64,
}

impl PciWindow {
    /// `size` ports from `base`.
    pub const fn io(base: u64, size: u64) -> Self {
        Self {
            kind: PciWindowKind::Io,
            base,
            size,
        }
    }

    /// `size` bytes of memory from `base`.
    pub const fn mmio(base: u64, size: u64) -> Self {
        Self {
            kind: PciWindowKind::Mmio,
            base,
            size,
        }
    }

    /// Exclusive end address (`base + size`), saturating at `u64::MAX`.
    pub const fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    /// Whether a 32-bit BAR can target this window.
    pub const fn is_below_4g(&self) -> bool {
        self.base.saturating_add(self.size) <= 0x1_0000_0000
    }
}
