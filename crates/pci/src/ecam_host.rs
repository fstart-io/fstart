//! PCI ECAM host bridge driver with bus enumeration and resource allocation.
//!
//! This driver implements a PCIe root complex that uses the Enhanced
//! Configuration Access Mechanism (ECAM) for config-space access.  On
//! `init()` it performs a full bus walk, sizes every BAR, allocates
//! resources from what its root bridges say it may use ([`PciRoot`]), programs
//! the BARs and bridge forwarding windows, and enables memory/IO decode.
//!
//! The allocation algorithm follows coreboot's key domain-level rules:
//! chipset-owned resources are explicit and removed from free space, and
//! movable BARs are placed largest-alignment-first. Like coreboot's, it
//! sizes each bridge's windows from its subtree, bottom up, then places
//! them top down, everything behind a bridge inside its windows. Where
//! that finds no layout, a bounded search over the windows does.
//!
//! One allocator can own several root bridges (PCI domains):
//! each has its own bus range and windows, and the devices below it get
//! their bus numbers and resources from those alone.
//!
//! Compatible: `"pci-host-ecam-generic"`.

// The production allocator uses bounded heapless storage. Only host tests
// need alloc; importing it in firmware would unnecessarily require a heap.
#[cfg(test)]
extern crate alloc;

use heapless::Vec as HVec;
use tock_registers::interfaces::ReadWriteable;

use crate::{
    ConfigRegionAccess, PCI_BAR0, PCI_CMD_BUS_MASTER, PCI_CMD_IO, PCI_CMD_MEMORY, PCI_COMMAND,
    PCI_HEADER_TYPE, PCI_HEADER_TYPE_BRIDGE, PCI_HEADER_TYPE_CARDBUS, PCI_HEADER_TYPE_MULTI_FUNC,
    PCI_IO_BASE, PCI_MEMORY_BASE, PCI_PREF_BASE_UPPER32, PCI_PREF_LIMIT_UPPER32,
    PCI_PREF_MEMORY_BASE, PCI_PRIMARY_BUS, PCI_VENDOR_ID, PCI_VENDOR_INVALID, PciAddress, PciRoot,
    PciWindow, PciWindowKind,
};

// -----------------------------------------------------------------------
// Internal types
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum PciEcamError {
    ConfigError,
    ResourceRangeLimit,
    FixedBarInvalid,
    FixedBarConflict,
    ResourceExhausted,
    /// The window search gave up, after a fixed number of candidate
    /// windows, before it found a layout or proved there is none.
    SearchLimit,
}

impl PciEcamError {
    fn log_bar(self, operation: &str, address: PciAddress, register: u16) {
        fstart_log::error!(
            "pci: {} failed segment={:#x} bus={:#x} device={:#x} function={} BAR={:#x}: {:?}",
            operation,
            address.segment(),
            address.bus(),
            address.device(),
            address.function(),
            register,
            self
        );
    }
}

/// Address-space type of a chipset-owned PCI BAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciFixedBarType {
    Io,
    Memory32,
    Memory64,
}

/// A PCI BAR whose address is fixed by chipset policy rather than allocated.
///
/// The owning chipset driver supplies the authoritative base and size. The
/// allocator validates the live BAR, removes the interval from its free pool,
/// and never probes or rewrites the register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciFixedBar {
    pub address: PciAddress,
    pub register: u16,
    pub kind: PciFixedBarType,
    pub base: u64,
    pub size: u64,
    pub prefetchable: bool,
}

/// Maximum number of fixed BARs declared by one platform.
pub const MAX_PCI_FIXED_BARS: usize = 16;
pub type PciFixedBars = HVec<PciFixedBar, MAX_PCI_FIXED_BARS>;

/// BAR type after sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarType {
    None,
    Io,
    Memory32,
    Memory64,
}

/// A sized BAR and, once the allocator has placed it, its address.
#[derive(Debug, Clone, Copy)]
struct BarInfo {
    bar_type: BarType,
    size: u64,
    prefetchable: bool,
    /// BAR register offset (0x10..0x24).
    reg: u16,
    /// Whether the allocator has placed this BAR (at `base`), or the chipset
    /// has (a fixed BAR).
    allocated: bool,
    /// The chipset owns this BAR's address; generic allocation must preserve it.
    fixed: bool,
    /// Where the allocator placed it; written to the BAR once every BAR of
    /// every root is placed.
    base: u64,
    /// For a 64-bit prefetchable BAR behind bridges: which side of 4 GiB.
    side: Side,
}

/// Which side of 4 GiB a 64-bit prefetchable BAR behind a bridge goes to.
/// The allocator decides it BAR by BAR before it places the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Low,
    High,
    /// Above 4 GiB until the allocator tries it below.
    Untried,
}

/// What a type-1 bridge's prefetchable window can forward: the low nibble of
/// its Prefetchable Memory Base register (PCI-to-PCI Bridge 1.2, 3.2.5.9),
/// and whether its upper base register holds bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefetchWindow {
    /// The bridge has none: prefetchable memory behind it goes through its
    /// memory window.
    None,
    /// Below 4 GiB only.
    Bits32,
    /// Anywhere in the 64-bit space.
    Bits64,
}

/// A discovered PCI device or bridge.
struct PciDev {
    addr: PciAddress,
    /// Index of its root bridge.
    root: usize,
    header_type: u8,
    /// Command register before BAR probing temporarily disabled decode.
    original_command: u16,
    bars: [BarInfo; 6],
    /// For bridges: secondary bus number.
    secondary_bus: u8,
    /// For bridges: subordinate bus number.
    subordinate_bus: u8,
    /// For type-1 bridges: the windows, by [`WindowKind`].
    windows: [Window; WINDOW_KINDS.len()],
    /// For type-1 bridges: what its prefetchable window can forward.
    prefetch: PrefetchWindow,
    /// The bridge whose secondary bus it is on; `None` on the root bus.
    parent: Option<usize>,
    /// The allocator wrote its BARs, its windows and its command register:
    /// no BAR or window of it decodes an address it did not place.
    programmed: bool,
}

/// The windows a type-1 bridge forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Io = 0,
    /// The 32-bit non-prefetchable memory window.
    Memory = 1,
    /// The prefetchable memory window, 32- or 64-bit ([`PrefetchWindow`]).
    Prefetchable = 2,
}

const WINDOW_KINDS: [WindowKind; 3] =
    [WindowKind::Io, WindowKind::Memory, WindowKind::Prefetchable];

impl WindowKind {
    /// The granularity of the window's base and limit registers.
    const fn granularity(self) -> u64 {
        match self {
            Self::Io => 0x1000,
            Self::Memory | Self::Prefetchable => 0x10_0000,
        }
    }

    /// Its base register, for the log.
    const fn register(self) -> u16 {
        match self {
            Self::Io => PCI_IO_BASE,
            Self::Memory => PCI_MEMORY_BASE,
            Self::Prefetchable => PCI_PREF_MEMORY_BASE,
        }
    }
}

/// A bridge window: sized from what is behind the bridge, then placed.
/// Closed when its size is 0.
///
/// What is behind it is laid out one of two ways, largest alignment first:
/// up from a base that is a multiple of its largest alignment, or down
/// from an end that is ([`Self::down`]). Down, the alignment gap is below
/// the window, not in it: a smaller BAR before it in the pool fills it,
/// as one did when BARs were placed one by one. Every window below
/// one laid out down is laid out down too.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    base: u64,
    /// Its size as it is placed: laid out up until then.
    size: u64,
    /// Its size laid out up.
    size_up: u64,
    /// Its size laid out down.
    size_down: u64,
    align: u64,
    placed: bool,
    /// How what is behind it is laid out in it.
    layout: Layout,
    /// The prefetchable window holds something above 4 GiB.
    high: bool,
}

/// How what is behind a window is laid out in it ([`Window`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Layout {
    #[default]
    Up,
    Down,
    /// Its span, when BARs are placed one by one
    /// ([`PciEcam::place_bar_by_bar`]).
    Span,
}

impl Window {
    /// Its size laid out up or down, and where its base goes relative to a
    /// multiple of its alignment.
    fn layout(&self, down: bool) -> (u64, u64) {
        if down {
            let phase = self.size_down.wrapping_neg() & (self.align - 1);
            (self.size_down, phase)
        } else {
            (self.size_up, 0)
        }
    }
}

/// What a function asks of the window or root pool it is placed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    /// BAR `.1` of device `.0`.
    Bar(usize, usize),
    /// A window of bridge `.0`.
    Window(usize, WindowKind),
}

/// A device's requests with their sizes and alignments: six BARs and a
/// bridge's windows at most.
type DeviceRequests = HVec<(Request, u64, u64), { 6 + WINDOW_KINDS.len() }>;

/// A pool of a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pool {
    Io,
    Low,
    High,
}

impl Root {
    fn pool(&mut self, pool: Pool) -> &mut ResourcePool {
        match pool {
            Pool::Io => &mut self.io_pool,
            Pool::Low => &mut self.mmio32,
            Pool::High => &mut self.mmio64,
        }
    }
}

/// The order BARs are placed in one by one: bus (lowest first), whether
/// it only fits below 4 GiB, alignment.
type BarRank = (core::cmp::Reverse<u8>, bool, u64);

/// A root's free memory below and above 4 GiB, and its free ports.
type Pools = (ResourcePool, ResourcePool, ResourcePool);

/// The root pools a request may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Io,
    /// Below 4 GiB only.
    Low,
    /// Above 4 GiB only.
    High,
    /// Below 4 GiB if it fits, else above.
    LowOrHigh,
}

impl Placement {
    /// The pool's name, for the log.
    const fn pool(self) -> &'static str {
        match self {
            Self::Io => "I/O",
            Self::Low => "below-4-GiB memory",
            Self::High => "above-4-GiB memory",
            Self::LowOrHigh => "memory",
        }
    }
}

/// The diagnostic path shares the allocator's bounded bus walk, but journals
/// routing changes and never probes BARs or changes command/decode policy.
enum Scan<'a> {
    Resources {
        targets: Option<&'a [PciAddress]>,
        exact: bool,
    },
    ConfigOnly(ConfigScan<'a>),
}
struct ConfigScan<'a> {
    routes: &'a mut RouteJournal,
    targets: &'a [PciAddress],
    exact: bool,
}

impl Scan<'_> {
    fn save_route(&mut self, device: crate::EcamDevice, original: u32) -> Result<(), PciEcamError> {
        if let Self::ConfigOnly(config) = self {
            config
                .routes
                .0
                .push((device, original))
                .map_err(|_| PciEcamError::ConfigError)?;
        }
        Ok(())
    }
}

struct RouteJournal(HVec<(crate::EcamDevice, u32), MAX_PCI_DEVICES>);
impl RouteJournal {
    fn restore(&mut self) -> Result<(), PciEcamError> {
        let mut exact = true;
        // Children first: their config space disappears when the parent
        // route is restored. Attempt every restoration even after a fault.
        while let Some((device, original)) = self.0.pop() {
            device.write32(PCI_PRIMARY_BUS, original);
            exact &= device.read32(PCI_PRIMARY_BUS) == original;
        }
        if exact {
            Ok(())
        } else {
            Err(PciEcamError::ConfigError)
        }
    }
}
impl Drop for RouteJournal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Maximum number of free intervals kept for one resource type.
const MAX_RESOURCE_RANGES: usize = 16;

#[derive(Debug, Clone, Copy)]
struct ResourceRange {
    base: u64,
    end: u64,
}

/// Free-range allocator for one address type (MMIO32, MMIO64, or IO).
///
/// Unlike a plain bump allocator, this preserves holes occupied by fixed
/// chipset resources such as ECAM, and alignment gaps between BARs. This
/// matches coreboot's domain allocator, which subtracts fixed resources
/// before placing dynamic BARs.
#[derive(Debug, Clone, Copy)]
struct ResourcePool {
    ranges: [ResourceRange; MAX_RESOURCE_RANGES],
    count: usize,
}

const EMPTY_RESOURCE_RANGE: ResourceRange = ResourceRange { base: 0, end: 0 };

fn fixed_bars_overlap(left: &PciFixedBar, right: &PciFixedBar) -> bool {
    let same_space = matches!(
        (left.kind, right.kind),
        (PciFixedBarType::Io, PciFixedBarType::Io)
            | (
                PciFixedBarType::Memory32 | PciFixedBarType::Memory64,
                PciFixedBarType::Memory32 | PciFixedBarType::Memory64
            )
    );
    same_space
        && left.base < right.base.saturating_add(right.size)
        && right.base < left.base.saturating_add(left.size)
}

/// No movable bits means no BAR, except a 64-bit memory BAR of 4 GiB or
/// more: its low half has no movable address bits, only its type (64-bit
/// memory) identifies it, and its size lives in the upper half, which is
/// never a BAR of its own.
const fn bar_unimplemented(ones: u32, moving_lo: u32) -> bool {
    moving_lo == 0 && ones & 0x7 != 0x4
}

fn size_from_moving_bits(moving: u64, address_mask: u64) -> Result<u64, PciEcamError> {
    let address_bits = moving & address_mask;
    if address_bits == 0 {
        return Err(PciEcamError::ConfigError);
    }
    Ok(1u64 << address_bits.trailing_zeros())
}

impl ResourcePool {
    const fn empty() -> Self {
        Self {
            ranges: [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES],
            count: 0,
        }
    }

    #[cfg(test)]
    fn new(base: u64, size: u64) -> Result<Self, PciEcamError> {
        let mut pool = Self {
            ranges: [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES],
            count: 0,
        };
        let end = base.checked_add(size).ok_or(PciEcamError::ConfigError)?;
        pool.push_range(base, end)?;
        Ok(pool)
    }

    fn push_range(&mut self, base: u64, end: u64) -> Result<(), PciEcamError> {
        if base >= end {
            return Ok(());
        }
        if self.count == MAX_RESOURCE_RANGES {
            return Err(PciEcamError::ResourceRangeLimit);
        }
        self.ranges[self.count] = ResourceRange { base, end };
        self.count += 1;
        Ok(())
    }

    /// Remove a fixed address interval from the free space.
    fn reserve_range(&mut self, base: u64, size: u64) -> Result<(), PciEcamError> {
        if size == 0 || self.count == 0 {
            return Ok(());
        }
        let end = base.checked_add(size).ok_or(PciEcamError::ConfigError)?;
        let old_ranges = self.ranges;
        let old_count = self.count;
        self.ranges = [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES];
        self.count = 0;

        for range in old_ranges.into_iter().take(old_count) {
            if end <= range.base || base >= range.end {
                self.push_range(range.base, range.end)?;
                continue;
            }
            if range.base < base {
                self.push_range(range.base, base.min(range.end))?;
            }
            if end < range.end {
                self.push_range(end.max(range.base), range.end)?;
            }
        }
        Ok(())
    }

    fn free_bytes(&self) -> u64 {
        self.ranges[..self.count]
            .iter()
            .map(|range| range.end - range.base)
            .sum()
    }

    /// Whether `[base, end)` is free.
    fn is_free(&self, base: u64, end: u64) -> bool {
        self.ranges[..self.count]
            .iter()
            .any(|range| range.base <= base && end <= range.end)
    }

    /// The size of its largest free range.
    fn largest(&self) -> u64 {
        self.ranges[..self.count]
            .iter()
            .map(|range| range.end - range.base)
            .max()
            .unwrap_or(0)
    }

    fn allocate_aligned(&mut self, size: u64, align: u64) -> Option<u64> {
        self.allocate_at(size, align, 0)
    }

    /// Allocate `size` bytes, first fit, at an address that is `phase` above
    /// a multiple of `align` (a power of two).
    fn allocate_at(&mut self, size: u64, align: u64, phase: u64) -> Option<u64> {
        if size == 0 || align == 0 || phase >= align {
            return None;
        }
        for idx in 0..self.count {
            let range = self.ranges[idx];
            let Some(aligned) = range
                .base
                .checked_add(phase.wrapping_sub(range.base) & (align - 1))
            else {
                continue;
            };
            let Some(end) = aligned.checked_add(size) else {
                continue;
            };
            if end > range.end {
                continue;
            }
            // Remove only the allocated interval, keeping its alignment
            // prefix available for smaller BARs. Commit atomically: if the
            // split exceeds the range capacity, try another candidate.
            let mut remaining = *self;
            if remaining.reserve_range(aligned, size).is_err() {
                continue;
            }
            *self = remaining;
            return Some(aligned);
        }
        None
    }
}

// -----------------------------------------------------------------------
// Driver struct
// -----------------------------------------------------------------------

/// Functions of all the root bridges of one allocator: two SP3 sockets have
/// about 160.
const MAX_PCI_DEVICES: usize = 256;

/// One root bridge: its buses in the ECAM and its allocation pools.
#[derive(Clone, Copy)]
struct Root {
    segment: u16,
    /// ECAM base of the segment: bus `b` is at `ecam_base + (b << 20)`.
    ecam_base: usize,
    /// Mapped ECAM bytes from bus `bus_start` on.
    ecam_size: usize,
    bus_start: u8,
    bus_end: u8,
    /// Free memory below 4 GiB, above, and free ports: what the allocator
    /// may use. Allocation leaves them as they were; where it put what is
    /// in the devices.
    mmio32: ResourcePool,
    mmio64: ResourcePool,
    io_pool: ResourcePool,
    /// Existing routes through this bus belong to a different owner.
    reserved_through: u8,
    /// Next bus number to assign to a bridge.
    next_bus: u16,
}

impl Root {
    fn contains(&self, addr: PciAddress) -> bool {
        addr.segment() == self.segment && (self.bus_start..=self.bus_end).contains(&addr.bus())
    }
}

/// The most root bridges one allocator owns.
pub const MAX_PCI_ROOTS: usize = 8;

/// PCI ECAM host bridge driver.
pub struct PciEcam {
    /// The root bridges as they were described, for the tables.
    described: HVec<PciRoot, MAX_PCI_ROOTS>,
    /// Their pools. At least one; the bus ranges of a segment don't overlap.
    roots: HVec<Root, MAX_PCI_ROOTS>,
    devices: HVec<PciDev, MAX_PCI_DEVICES>,
    fixed_bars: PciFixedBars,
    auto_bus_master: bool,
    /// Which allocators run.
    #[cfg(test)]
    allocators: Allocators,
    /// Candidate windows and work units the last search spent.
    #[cfg(test)]
    spent: (usize, usize),
}

/// Which allocators a test runs: the fast paths alone (the subtree and
/// BAR-by-BAR layouts, each checked by [`PciEcam::validate_plan`]), the
/// window search alone, or both (as firmware does).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Allocators {
    FastPaths,
    Search(solver::Budget),
    Both(solver::Budget),
}

// SAFETY: MMIO registers are hardware-fixed addresses from the board metadata.
// The driver is used single-threaded during firmware init.
unsafe impl Send for PciEcam {}
unsafe impl Sync for PciEcam {}

impl PciEcam {
    /// Leave bus mastering disabled after allocation until a device driver
    /// owns its DMA. Must be selected before discovery/resource writes.
    pub fn disable_auto_bus_master(&mut self) -> Result<(), PciEcamError> {
        if !self.devices.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        self.auto_bus_master = false;
        Ok(())
    }

    /// Preserve an already-owned downstream bus prefix without changing the
    /// root bus or its ECAM addresses. New routes start strictly after it.
    /// The caller must exclude the retained bridges from resource discovery.
    pub fn reserve_bus_prefix(
        &mut self,
        segment: u16,
        root_bus: u8,
        through: u8,
    ) -> Result<(), PciEcamError> {
        if !self.devices.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        let root = self
            .roots
            .iter_mut()
            .find(|r| r.segment == segment && r.bus_start == root_bus)
            .ok_or(PciEcamError::ConfigError)?;
        if through < root.bus_start || through > root.bus_end {
            return Err(PciEcamError::ConfigError);
        }
        root.reserved_through = root.reserved_through.max(through);
        root.next_bus = u16::from(root.reserved_through) + 1;
        Ok(())
    }

    /// Replace the resource pools of the single root: how the tests size
    /// their windows.
    #[cfg(test)]
    pub(crate) fn configure_windows(
        &mut self,
        mmio32_base: u64,
        mmio32_size: u64,
        mmio64_base: u64,
        mmio64_size: u64,
        pio_base: u64,
        pio_size: u64,
    ) -> Result<(), PciEcamError> {
        if !self.fixed_bars.is_empty() || self.roots.len() != 1 {
            return Err(PciEcamError::ConfigError);
        }
        let root = &mut self.roots[0];
        root.mmio32 = ResourcePool::new(mmio32_base, mmio32_size)?;
        root.mmio64 = ResourcePool::new(mmio64_base, mmio64_size)?;
        root.io_pool = ResourcePool::new(pio_base, pio_size)?;
        Ok(())
    }

    /// Reserve a fixed MMIO range before enumeration.
    ///
    /// The range is removed from both 32-bit and 64-bit allocation pools of
    /// every root so platform-fixed resources cannot be overlapped by
    /// dynamic BARs.
    pub fn reserve_mmio_range(&mut self, base: u64, size: u64) -> Result<(), PciEcamError> {
        let mut roots = self.roots.clone();
        for root in &mut roots {
            root.mmio32.reserve_range(base, size)?;
            root.mmio64.reserve_range(base, size)?;
        }
        self.roots = roots;
        Ok(())
    }

    /// Exclude firmware-owned port I/O from dynamic BAR allocation.
    /// Like fixed BARs, these intervals remain in the root aperture; the board
    /// must also describe them as consumed resources to the OS.
    pub fn reserve_io_range(&mut self, base: u64, size: u64) -> Result<(), PciEcamError> {
        if base.checked_add(size).is_none_or(|end| end > 0x1_0000) {
            return Err(PciEcamError::ConfigError);
        }
        let mut roots = self.roots.clone();
        for root in &mut roots {
            root.io_pool.reserve_range(base, size)?;
        }
        self.roots = roots;
        Ok(())
    }

    /// Register chipset-owned BARs before enumeration.
    ///
    /// Fixed BARs remain visible in the root aperture reported to the OS, but
    /// their intervals are removed from the allocator's private free pools.
    pub fn add_fixed_bars(&mut self, bars: &[PciFixedBar]) -> Result<(), PciEcamError> {
        let mut roots = self.roots.clone();
        let mut fixed_bars = self.fixed_bars.clone();

        for bar in bars {
            let root = self.validate_fixed_bar(bar)?;
            let Root {
                mmio32,
                mmio64,
                io_pool,
                ..
            } = &mut roots[root];
            if fixed_bars
                .iter()
                .any(|current| current.address == bar.address && current.register == bar.register)
            {
                return Err(PciEcamError::FixedBarConflict);
            }
            if fixed_bars
                .iter()
                .any(|current| fixed_bars_overlap(current, bar))
            {
                return Err(PciEcamError::FixedBarConflict);
            }

            match bar.kind {
                PciFixedBarType::Io => io_pool.reserve_range(bar.base, bar.size)?,
                PciFixedBarType::Memory32 | PciFixedBarType::Memory64 => {
                    mmio32.reserve_range(bar.base, bar.size)?;
                    mmio64.reserve_range(bar.base, bar.size)?;
                }
            }
            fixed_bars
                .push(*bar)
                .map_err(|_| PciEcamError::ResourceRangeLimit)?;
        }

        self.roots = roots;
        self.fixed_bars = fixed_bars;
        Ok(())
    }

    /// The root of a valid fixed BAR.
    fn validate_fixed_bar(&self, bar: &PciFixedBar) -> Result<usize, PciEcamError> {
        let end = bar
            .base
            .checked_add(bar.size)
            .ok_or(PciEcamError::FixedBarInvalid)?;
        let root = self
            .root_of(bar.address)
            .ok_or(PciEcamError::FixedBarInvalid)?;
        if !(PCI_BAR0..=PCI_BAR0 + 5 * 4).contains(&bar.register)
            || !(bar.register - PCI_BAR0).is_multiple_of(4)
            || bar.size == 0
            || !bar.size.is_power_of_two()
            || bar.base == 0
            || bar.base & (bar.size - 1) != 0
            || (bar.kind == PciFixedBarType::Io && end > 0x1_0000)
            || (bar.kind == PciFixedBarType::Memory32 && end > 0x1_0000_0000)
            || (bar.kind == PciFixedBarType::Io && bar.prefetchable)
        {
            return Err(PciEcamError::FixedBarInvalid);
        }
        if bar.kind == PciFixedBarType::Memory64 && bar.register == PCI_BAR0 + 5 * 4 {
            return Err(PciEcamError::FixedBarInvalid);
        }
        Ok(root)
    }

    // -- ECAM helpers --

    /// The root bridge whose buses include `addr`.
    fn root_of(&self, addr: PciAddress) -> Option<usize> {
        self.roots.iter().position(|root| root.contains(addr))
    }

    /// Bind the existing device accessor to its root's validated mapping.
    /// Invalid topology or an incomplete function mapping is not an absent device.
    pub fn device(&self, addr: PciAddress) -> Option<crate::EcamDevice> {
        let root = &self.roots[self.root_of(addr)?];
        let offset = crate::ecam::function_offset(addr);
        let mapped = offset - (usize::from(root.bus_start) << 20);
        if mapped.checked_add(crate::ecam::FUNCTION_CONFIG_BYTES)? > root.ecam_size {
            return None;
        }
        Some(crate::EcamDevice::at_config_base(
            addr,
            root.ecam_base.checked_add(offset)?,
        ))
    }

    // Allocator paths construct BDFs within their validated root topology.
    // A bad address is a programming error, not an all-ones hardware response.
    fn read32(&self, addr: PciAddress, reg: u16) -> u32 {
        self.device(addr)
            .expect("PCI address outside ECAM")
            .read32(reg)
    }

    fn write32(&self, addr: PciAddress, reg: u16, val: u32) {
        self.device(addr)
            .expect("PCI address outside ECAM")
            .write32(reg, val);
    }

    fn read16(&self, addr: PciAddress, reg: u16) -> u16 {
        self.device(addr)
            .expect("PCI address outside ECAM")
            .read16(reg)
    }

    fn write16(&self, addr: PciAddress, reg: u16, val: u16) {
        self.device(addr)
            .expect("PCI address outside ECAM")
            .write16(reg, val);
    }

    // -- BAR sizing --

    /// Size a single BAR.  Returns the BAR info and whether it consumed
    /// two BAR slots (64-bit).
    fn size_bar(&self, addr: PciAddress, bar_idx: usize) -> Result<(BarInfo, bool), PciEcamError> {
        let reg = PCI_BAR0 + (bar_idx as u16) * 4;
        let original = self.read32(addr, reg);

        if let Some(fixed) = self
            .fixed_bars
            .iter()
            .find(|fixed| fixed.address == addr && fixed.register == reg)
        {
            let original_base = match fixed.kind {
                PciFixedBarType::Io if original & 1 == 1 => u64::from(original & 0x0000_FFFC),
                PciFixedBarType::Memory32 if original & 1 == 0 && (original >> 1) & 0x3 == 0 => {
                    u64::from(original & 0xFFFF_FFF0)
                }
                PciFixedBarType::Memory64 if original & 1 == 0 && (original >> 1) & 0x3 == 2 => {
                    (u64::from(self.read32(addr, reg + 4)) << 32)
                        | u64::from(original & 0xFFFF_FFF0)
                }
                _ => return Err(PciEcamError::FixedBarInvalid),
            };
            if original_base != fixed.base
                || (fixed.kind != PciFixedBarType::Io
                    && ((original & 0x8) != 0) != fixed.prefetchable)
            {
                return Err(PciEcamError::FixedBarInvalid);
            }
            let bar_type = match fixed.kind {
                PciFixedBarType::Io => BarType::Io,
                PciFixedBarType::Memory32 => BarType::Memory32,
                PciFixedBarType::Memory64 => BarType::Memory64,
            };
            return Ok((
                BarInfo {
                    bar_type,
                    size: fixed.size,
                    prefetchable: fixed.prefetchable,
                    reg,
                    allocated: true,
                    fixed: true,
                    base: fixed.base,
                    side: Side::Low,
                },
                fixed.kind == PciFixedBarType::Memory64,
            ));
        }

        // Decode is disabled for this function by probe_device(). Probe both
        // all-ones and all-zeroes: their XOR identifies bits software can
        // move. A one-mask alone mis-sizes BARs with hardwired-one address
        // bits, including ICH7 SATA BAR4.
        self.write32(addr, reg, 0xFFFF_FFFF);
        let ones = self.read32(addr, reg);
        self.write32(addr, reg, 0);
        let zeroes = self.read32(addr, reg);
        self.write32(addr, reg, original);
        let moving_lo = ones ^ zeroes;

        let none = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg,
            allocated: false,
            fixed: false,
            base: 0,
            side: Side::Low,
        };

        if bar_unimplemented(ones, moving_lo) {
            return Ok((none, false));
        }

        let attributes = original & !moving_lo;
        if attributes & 1 == 1 {
            let size = size_from_moving_bits(u64::from(moving_lo), 0x0000_FFFC)?;
            return Ok((
                BarInfo {
                    bar_type: BarType::Io,
                    size,
                    prefetchable: false,
                    reg,
                    allocated: false,
                    fixed: false,
                    base: 0,
                    side: Side::Low,
                },
                false,
            ));
        }

        let prefetchable = (attributes & 0x8) != 0;
        match (attributes >> 1) & 0x3 {
            0 => {
                let size = size_from_moving_bits(u64::from(moving_lo), 0xFFFF_FFF0)?;
                Ok((
                    BarInfo {
                        bar_type: BarType::Memory32,
                        size,
                        prefetchable,
                        reg,
                        allocated: false,
                        fixed: false,
                        base: 0,
                        side: Side::Low,
                    },
                    false,
                ))
            }
            2 => {
                // Present both halves coherently while probing a 64-bit BAR.
                let upper_reg = reg + 4;
                let original_hi = self.read32(addr, upper_reg);
                self.write32(addr, reg, 0xFFFF_FFFF);
                self.write32(addr, upper_reg, 0xFFFF_FFFF);
                let ones_lo = self.read32(addr, reg);
                let ones_hi = self.read32(addr, upper_reg);
                self.write32(addr, reg, 0);
                self.write32(addr, upper_reg, 0);
                let zeroes_lo = self.read32(addr, reg);
                let zeroes_hi = self.read32(addr, upper_reg);
                self.write32(addr, upper_reg, original_hi);
                self.write32(addr, reg, original);

                let moving =
                    (u64::from(ones_hi ^ zeroes_hi) << 32) | u64::from(ones_lo ^ zeroes_lo);
                let size = size_from_moving_bits(moving, 0xFFFF_FFFF_FFFF_FFF0)?;
                Ok((
                    BarInfo {
                        bar_type: BarType::Memory64,
                        size,
                        prefetchable,
                        reg,
                        allocated: false,
                        fixed: false,
                        base: 0,
                        side: Side::Low,
                    },
                    true,
                ))
            }
            _ => Ok((none, false)),
        }
    }

    /// Probe a single device/function, size its BARs.
    fn probe_device(
        &self,
        addr: PciAddress,
        probe_bars: bool,
    ) -> Result<Option<PciDev>, PciEcamError> {
        let vendor_device = self.read32(addr, PCI_VENDOR_ID);
        if vendor_device == PCI_VENDOR_INVALID {
            return Ok(None);
        }
        let device = self.device(addr).ok_or(PciEcamError::ConfigError)?;
        let header_type = device.read8(PCI_HEADER_TYPE) & 0x7f;
        let base_class = (self.read32(addr, 0x08) >> 24) as u8;

        let max_bars = match header_type {
            PCI_HEADER_TYPE_BRIDGE => 2,
            // PCI-to-CardBus bridges use header type 2. Only BAR0 is a base
            // address register; offsets that look like BAR2..BAR5 are CardBus
            // bus/window registers and must not be sized as endpoint BARs.
            PCI_HEADER_TYPE_CARDBUS => 1,
            // Host and ISA bridges commonly use type-0-looking config space,
            // but their 0x10..0x24 registers are chipset-specific rather than
            // generic movable BARs. Their drivers own those resources.
            _ if base_class == 0x06 => 0,
            _ => 6,
        };

        let none_bar = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg: 0,
            allocated: false,
            fixed: false,
            base: 0,
            side: Side::Low,
        };
        let mut bars = [none_bar; 6];

        // BAR sizing is only safe while the function cannot decode memory or
        // I/O cycles. Restore the chipset's original policy immediately after
        // probing; allocation disables decode again around each BAR write.
        let command = self.read16(addr, PCI_COMMAND);
        if probe_bars {
            self.write16(addr, PCI_COMMAND, command & !(PCI_CMD_IO | PCI_CMD_MEMORY));

            let sizing_result = (|| {
                let mut i = 0;
                while i < max_bars {
                    let (info, is_64) = self.size_bar(addr, i).inspect_err(|error| {
                        error.log_bar("BAR probe", addr, PCI_BAR0 + i as u16 * 4);
                    })?;
                    bars[i] = info;
                    if is_64 {
                        i += 1; // skip upper half
                    }
                    i += 1;
                }
                Ok(())
            })();
            self.write16(addr, PCI_COMMAND, command);
            sizing_result?;
        }
        let prefetch = if probe_bars && header_type == PCI_HEADER_TYPE_BRIDGE {
            self.prefetch_window(addr)
        } else {
            PrefetchWindow::None
        };

        Ok(Some(PciDev {
            addr,
            root: self.root_of(addr).ok_or(PciEcamError::ConfigError)?,
            header_type,
            original_command: command,
            bars,
            secondary_bus: 0,
            subordinate_bus: 0,
            windows: Default::default(),
            prefetch,
            parent: None,
            programmed: false,
        }))
    }

    /// What the prefetchable window of bridge `addr` can forward, as Linux
    /// reads it (`pci_read_bridge_windows`): a base register that reads 0
    /// is tried with ones, then put back; a 64-bit one must hold bits in
    /// its upper base register. The allocator rewrites both later.
    fn prefetch_window(&self, addr: PciAddress) -> PrefetchWindow {
        let mut base = self.read32(addr, PCI_PREF_MEMORY_BASE);
        if base == 0 {
            self.write32(addr, PCI_PREF_MEMORY_BASE, 0xfff0_fff0);
            base = self.read32(addr, PCI_PREF_MEMORY_BASE);
            self.write32(addr, PCI_PREF_MEMORY_BASE, 0);
        }
        if base == 0 {
            return PrefetchWindow::None;
        }
        // Prefetchable Base, bits 3:0: 0 a 32-bit window, 1 a 64-bit one.
        if base & 0xf != 1 {
            return PrefetchWindow::Bits32;
        }
        let upper = self.read32(addr, PCI_PREF_BASE_UPPER32);
        self.write32(addr, PCI_PREF_BASE_UPPER32, u32::MAX);
        let ones = self.read32(addr, PCI_PREF_BASE_UPPER32);
        self.write32(addr, PCI_PREF_BASE_UPPER32, upper);
        if ones == 0 {
            PrefetchWindow::Bits32
        } else {
            PrefetchWindow::Bits64
        }
    }

    // -- Enumeration --

    /// Enumerate a bus of root `root` recursively.  Discovers devices,
    /// assigns bus numbers of the root to bridges, and recurses behind them.
    fn enumerate_bus(
        &mut self,
        root: usize,
        bus: u8,
        mode: &mut Scan,
        single_device: bool,
    ) -> Result<(), PciEcamError> {
        use crate::PCI_BRIDGE_BUSES;
        use tock_registers::LocalRegisterCopy;
        let Root {
            segment, bus_end, ..
        } = self.roots[root];
        if matches!(mode, Scan::ConfigOnly(_)) {
            fstart_log::info!("pci: config-only enter bus={:#x}", bus);
        }
        // The next bus number of this root, if it has one left.
        let next_bus = |pci: &Self| {
            u8::try_from(pci.roots[root].next_bus)
                .ok()
                .filter(|&next| next <= bus_end)
                .ok_or(PciEcamError::ResourceExhausted)
        };
        let selected = match mode {
            Scan::ConfigOnly(config) if config.exact || bus == self.roots[root].bus_start => {
                Some(config.targets)
            }
            Scan::Resources { targets, exact } if *exact || bus == self.roots[root].bus_start => {
                *targets
            }
            _ => None,
        };
        // PCIe root/downstream ports are point-to-point: only device0 can
        // exist on the secondary bus. Never send impossible Type0 requests.
        for dev in 0..if single_device { 1u8 } else { 32u8 } {
            let max_func = if let Some(ports) = selected {
                if !ports
                    .iter()
                    .any(|p| p.segment() == segment && p.bus() == bus && p.device() == dev)
                {
                    continue;
                }
                8
            } else {
                let addr = PciAddress::new(segment, bus, dev, 0);
                if self.read32(addr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                    continue;
                }
                let device = self.device(addr).ok_or(PciEcamError::ConfigError)?;
                if device.read8(PCI_HEADER_TYPE) & PCI_HEADER_TYPE_MULTI_FUNC != 0 {
                    8
                } else {
                    1
                }
            };

            for func in 0..max_func {
                let faddr = PciAddress::new(segment, bus, dev, func);
                if selected.is_some_and(|ports| !ports.contains(&faddr)) {
                    continue;
                }
                if matches!(mode, Scan::ConfigOnly(_)) {
                    fstart_log::info!(
                        "pci: config-only reading bus={:#x} device={:#x} function={}",
                        bus,
                        dev,
                        func
                    );
                }
                if (func > 0 || selected.is_some())
                    && self.read32(faddr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID
                {
                    continue;
                }

                if let Some(mut pci_dev) =
                    self.probe_device(faddr, matches!(mode, Scan::Resources { .. }))?
                {
                    if matches!(mode, Scan::ConfigOnly(_)) {
                        fstart_log::info!(
                            "pci: config-only function bus={:#x} device={:#x} function={} header={:#x}",
                            bus,
                            dev,
                            func,
                            pci_dev.header_type
                        );
                    }
                    if matches!(mode,Scan::ConfigOnly(config) if !config.exact)
                        && bus == self.roots[root].bus_start
                        && pci_dev.header_type != PCI_HEADER_TYPE_BRIDGE
                    {
                        return Err(PciEcamError::ConfigError);
                    }
                    match pci_dev.header_type {
                        PCI_HEADER_TYPE_BRIDGE => {
                            let secondary = next_bus(self)?;
                            self.roots[root].next_bus += 1;
                            pci_dev.secondary_bus = secondary;

                            let device = self.device(faddr).ok_or(PciEcamError::ConfigError)?;
                            let mut buses =
                                LocalRegisterCopy::<u32, PCI_BRIDGE_BUSES::Register>::new(
                                    device.read32(PCI_PRIMARY_BUS),
                                );
                            mode.save_route(device, buses.get())?;
                            // Preserve the secondary latency byte.
                            buses.modify(
                                PCI_BRIDGE_BUSES::PRIMARY.val(bus.into())
                                    + PCI_BRIDGE_BUSES::SECONDARY.val(secondary.into())
                                    + PCI_BRIDGE_BUSES::SUBORDINATE.val(bus_end.into()),
                            );
                            self.write32(faddr, PCI_PRIMARY_BUS, buses.get());

                            let single_device=device.find_capability(0x10).filter(|&c|c+4<=0x100).is_some_and(|c| {
                                let caps=LocalRegisterCopy::<u16,crate::PCI_EXPRESS_CAPABILITIES::Register>::new(device.read16(c+2));
                                caps.matches_all(crate::PCI_EXPRESS_CAPABILITIES::DEVICE_PORT_TYPE::RootPort) || caps.matches_all(crate::PCI_EXPRESS_CAPABILITIES::DEVICE_PORT_TYPE::DownstreamPort)
                            });
                            self.enumerate_bus(root, secondary, mode, single_device)?;

                            // Finalise subordinate = highest bus found.
                            pci_dev.subordinate_bus = (self.roots[root].next_bus - 1) as u8;
                            buses.modify(
                                PCI_BRIDGE_BUSES::SUBORDINATE.val(pci_dev.subordinate_bus.into()),
                            );
                            self.write32(faddr, PCI_PRIMARY_BUS, buses.get());
                        }
                        PCI_HEADER_TYPE_CARDBUS => {
                            let secondary = next_bus(self)?;
                            // CardBus bridges need a bus-number range for
                            // cards inserted later. Reserve the conventional
                            // four-bus window used by Linux/coreboot rather
                            // than leaving the bridge at [bus 00-00].
                            let subordinate = secondary.saturating_add(3).min(bus_end);
                            self.roots[root].next_bus = u16::from(subordinate) + 1;
                            pci_dev.secondary_bus = secondary;
                            pci_dev.subordinate_bus = subordinate;
                            let device = self.device(faddr).ok_or(PciEcamError::ConfigError)?;
                            let mut buses =
                                LocalRegisterCopy::<u32, PCI_BRIDGE_BUSES::Register>::new(
                                    device.read32(PCI_PRIMARY_BUS),
                                );
                            mode.save_route(device, buses.get())?;
                            buses.modify(
                                PCI_BRIDGE_BUSES::PRIMARY.val(bus.into())
                                    + PCI_BRIDGE_BUSES::SECONDARY.val(secondary.into())
                                    + PCI_BRIDGE_BUSES::SUBORDINATE.val(subordinate.into()),
                            );
                            self.write32(faddr, PCI_PRIMARY_BUS, buses.get());
                        }
                        _ => {}
                    }

                    if self.devices.push(pci_dev).is_err() {
                        return Err(PciEcamError::ConfigError);
                    }
                    if matches!(mode, Scan::ConfigOnly(_)) {
                        fstart_log::info!("pci: config-only metadata stored");
                    }
                }
            }
        }
        Ok(())
    }

    // -- Resource allocation --
    //
    // Two passes over the tree, as coreboot's allocator does. Bottom up,
    // each bridge's three windows are sized from what is behind it: the
    // BARs on its secondary bus and the windows of the bridges there, laid
    // out largest alignment first, each at its own alignment, up or down
    // ([`Window`]); only the window is rounded to its granularity (4 KiB of
    // ports, 1 MiB of memory). Top down, the root bus's BARs and its
    // bridges' windows are placed in the root's pools, and everything
    // behind a bridge goes inside the window placed for it. A bridge's
    // window therefore spans its subtree and nothing else: no sibling's
    // BAR, no root-bus BAR, no fixed BAR.
    //
    // Where no such layout fits, the root is placed BAR by BAR as before
    // (each BAR behind a bridge aligned to the window's granularity), and
    // that is kept if the windows it implies decode as meant
    // ([`Self::place_bar_by_bar`]): whatever fitted before still does.
    // Where neither fits, a search chooses the windows and packs each bus
    // exactly ([`solver`]); it finds what no greedy move does, or proves
    // there is nothing, or says it gave up. Whichever layout is kept is
    // checked against the root's pools as they were before
    // ([`Self::validate_plan`]).
    //
    // Prefetchable memory takes the windows the BAR-by-BAR allocator before
    // it derived from where it put each BAR. A 64-bit prefetchable BAR
    // behind bridges whose prefetchable windows are all 64-bit goes below
    // 4 GiB while the root has room for it there, in bus order and largest
    // first, else above ([`Self::allocate_root`]). A bridge with such a BAR
    // above 4 GiB behind it forwards those through its prefetchable window,
    // above 4 GiB, and the rest of its prefetchable memory through its
    // memory window. Otherwise its prefetchable window, below 4 GiB, takes
    // all of it, 32-bit BARs too. A bridge without a prefetchable window
    // forwards prefetchable memory through its memory window (prefetchable
    // memory may always be forwarded as non-prefetchable).
    //
    // No BAR, window or command register is written until every root is
    // placed: an allocation that fails leaves them as it found them.
    // Discovery before it is not undone: it numbered the bridges' buses
    // (and wrote BARs to size them, each put back). A failure is fail-stop:
    // the hierarchy is left routed for a diagnosis, not for a retry.

    /// What device `dev_idx` asks of its parent, each with its size and
    /// alignment: its movable BARs and, for a bridge, its open windows.
    fn requests_of(&self, dev_idx: usize) -> DeviceRequests {
        let device = &self.devices[dev_idx];
        let bars = device
            .bars
            .iter()
            .enumerate()
            .filter(|(_, bar)| bar.bar_type != BarType::None && !bar.fixed)
            .map(|(bar_idx, bar)| (Request::Bar(dev_idx, bar_idx), bar.size, bar.size));
        let windows = WINDOW_KINDS
            .into_iter()
            .filter(|_| device.header_type == PCI_HEADER_TYPE_BRIDGE)
            .map(|kind| (kind, device.windows[kind as usize]))
            .filter(|(_, window)| window.size != 0)
            .map(|(kind, window)| (Request::Window(dev_idx, kind), window.size, window.align));
        bars.chain(windows).collect()
    }

    /// Whether `request` is prefetchable memory, and whether it goes above
    /// 4 GiB.
    fn prefetchable(&self, request: Request) -> (bool, bool) {
        match request {
            Request::Bar(dev_idx, bar_idx) => {
                let bar = self.devices[dev_idx].bars[bar_idx];
                (
                    bar.prefetchable && bar.bar_type != BarType::Io,
                    matches!(bar.side, Side::High | Side::Untried),
                )
            }
            Request::Window(dev_idx, WindowKind::Prefetchable) => (
                true,
                self.devices[dev_idx].windows[WindowKind::Prefetchable as usize].high,
            ),
            Request::Window(..) => (false, false),
        }
    }

    /// The window of `bridge` that `request`, on its secondary bus, goes
    /// through.
    fn window_for(&self, bridge: usize, request: Request) -> WindowKind {
        let io = match request {
            Request::Bar(dev_idx, bar_idx) => {
                self.devices[dev_idx].bars[bar_idx].bar_type == BarType::Io
            }
            Request::Window(_, kind) => kind == WindowKind::Io,
        };
        let (prefetchable, high) = self.prefetchable(request);
        let bridge = &self.devices[bridge];
        if io {
            WindowKind::Io
        } else if high
            || prefetchable
                && bridge.prefetch != PrefetchWindow::None
                && !bridge.windows[WindowKind::Prefetchable as usize].high
        {
            WindowKind::Prefetchable
        } else {
            WindowKind::Memory
        }
    }

    fn placed(&self, request: Request) -> bool {
        match request {
            Request::Bar(dev_idx, bar_idx) => self.devices[dev_idx].bars[bar_idx].allocated,
            Request::Window(dev_idx, kind) => self.devices[dev_idx].windows[kind as usize].placed,
        }
    }

    /// Put `request` at `base`: the BAR's address, or the window's base for
    /// what goes behind it, laid out down or up.
    fn assign(&mut self, request: Request, base: u64, layout: Layout) {
        match request {
            Request::Bar(dev_idx, bar_idx) => {
                let bar = &mut self.devices[dev_idx].bars[bar_idx];
                bar.base = base;
                bar.allocated = true;
            }
            Request::Window(dev_idx, kind) => {
                let window = &mut self.devices[dev_idx].windows[kind as usize];
                window.size = window.layout(layout == Layout::Down).0;
                window.base = base;
                window.placed = true;
                window.layout = layout;
            }
        }
    }

    /// The size, alignment and phase (where its base goes relative to a
    /// multiple of its alignment) of `request` laid out down or up.
    fn layout(&self, request: Request, down: bool) -> (u64, u64, u64) {
        match request {
            Request::Bar(dev_idx, bar_idx) => {
                let size = self.devices[dev_idx].bars[bar_idx].size;
                (size, size, 0)
            }
            Request::Window(dev_idx, kind) => {
                let window = self.devices[dev_idx].windows[kind as usize];
                let (size, phase) = window.layout(down);
                (size, window.align, phase)
            }
        }
    }

    /// The devices on the secondary bus of `bridge`.
    fn children(&self, bridge: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.devices.len()).filter(move |&dev_idx| self.devices[dev_idx].parent == Some(bridge))
    }

    /// Lay out what goes through window `kind` of `bridge`, largest
    /// alignment first, each at the next address its alignment and phase
    /// allow: up from `base`, or down from the end of a window at `base`
    /// whose end is a multiple of every alignment in it. Place it when
    /// `place` is set. Of one alignment, what is a multiple of it in size
    /// goes first: a window that is not leaves the next one misaligned.
    /// Returns the bytes used and the largest alignment.
    ///
    /// A window's base (up) or end (down) is a multiple of every alignment
    /// behind it, so the layout sized from 0 is the one placed.
    fn lay_out(
        &mut self,
        bridge: usize,
        kind: WindowKind,
        base: u64,
        place: bool,
        down: bool,
    ) -> Result<(u64, u64), PciEcamError> {
        let mut alignments = 0u64;
        for dev_idx in self.children(bridge) {
            for (request, _, align) in self.requests_of(dev_idx) {
                if self.window_for(bridge, request) == kind {
                    alignments |= align;
                }
            }
        }
        let largest = match alignments {
            0 => 0,
            _ => 1u64 << (63 - alignments.leading_zeros()),
        };
        // Up, the offset of the next free byte from `base`; down, of the
        // lowest used byte below the end.
        let mut used = 0u64;
        let end = if down && place {
            let window = self.devices[bridge].windows[kind as usize];
            base.checked_add(window.size)
                .ok_or(PciEcamError::ResourceExhausted)?
        } else {
            base
        };
        while alignments != 0 {
            let align = 1u64 << (63 - alignments.leading_zeros());
            alignments &= !align;
            for whole in [true, false] {
                for dev_idx in 0..self.devices.len() {
                    if self.devices[dev_idx].parent != Some(bridge) {
                        continue;
                    }
                    for (request, _, request_align) in self.requests_of(dev_idx) {
                        let (size, _, phase) = self.layout(request, down);
                        if request_align != align
                            || size.is_multiple_of(align) != whole
                            || self.window_for(bridge, request) != kind
                        {
                            continue;
                        }
                        let start = if down {
                            // Its end goes `phase + size` above a multiple.
                            let gap = phase.wrapping_add(size).wrapping_neg().wrapping_sub(used)
                                & (align - 1);
                            used = used
                                .checked_add(gap)
                                .and_then(|used| used.checked_add(size))
                                .ok_or(PciEcamError::ResourceExhausted)?;
                            end.wrapping_sub(used)
                        } else {
                            let cursor = base
                                .checked_add(used)
                                .ok_or(PciEcamError::ResourceExhausted)?;
                            let start = cursor
                                .checked_add(phase.wrapping_sub(cursor) & (align - 1))
                                .ok_or(PciEcamError::ResourceExhausted)?;
                            used = start
                                .checked_add(size)
                                .ok_or(PciEcamError::ResourceExhausted)?
                                - base;
                            start
                        };
                        if place {
                            let layout = if down { Layout::Down } else { Layout::Up };
                            self.assign(request, start, layout);
                        }
                    }
                }
            }
        }
        Ok((used, largest))
    }

    /// Size the windows of every bridge of root `root`, the deepest first:
    /// a bridge's buses come after its parent's, so its windows are sized
    /// before the parent's layout needs them.
    fn size_windows(&mut self, root: usize, bridges: &[usize]) -> Result<(), PciEcamError> {
        for &bridge in bridges.iter().rev() {
            if self.devices[bridge].root != root {
                continue;
            }
            self.devices[bridge].windows = Default::default();
            let high = self
                .children(bridge)
                .flat_map(|dev_idx| self.requests_of(dev_idx))
                .any(|(request, ..)| self.prefetchable(request).1);
            if high && self.devices[bridge].prefetch != PrefetchWindow::Bits64 {
                return Err(PciEcamError::ResourceExhausted);
            }
            for kind in WINDOW_KINDS {
                self.devices[bridge].windows[kind as usize].high =
                    kind == WindowKind::Prefetchable && high;
            }
            for kind in WINDOW_KINDS {
                let (up, largest) = self.lay_out(bridge, kind, 0, false, false)?;
                let (down, _) = self.lay_out(bridge, kind, 0, false, true)?;
                let granular = |used: u64| {
                    used.checked_next_multiple_of(kind.granularity())
                        .ok_or(PciEcamError::ResourceExhausted)
                };
                let window = &mut self.devices[bridge].windows[kind as usize];
                window.size = granular(up)?;
                window.size_up = window.size;
                window.size_down = granular(down)?;
                window.align = largest.max(kind.granularity());
                window.high &= window.size != 0;
            }
        }
        Ok(())
    }

    /// Which of the root's pools `request`, on the root bus, may come from.
    fn placement(&self, request: Request) -> Placement {
        match request {
            Request::Bar(dev_idx, bar_idx) => match self.devices[dev_idx].bars[bar_idx].bar_type {
                BarType::Io => Placement::Io,
                BarType::Memory64 => Placement::LowOrHigh,
                BarType::Memory32 | BarType::None => Placement::Low,
            },
            Request::Window(_, WindowKind::Io) => Placement::Io,
            Request::Window(_, WindowKind::Memory) => Placement::Low,
            Request::Window(dev_idx, WindowKind::Prefetchable) => {
                if self.devices[dev_idx].windows[WindowKind::Prefetchable as usize].high {
                    Placement::High
                } else {
                    Placement::Low
                }
            }
        }
    }

    /// Place the root bus of root `root`: its BARs and its bridges'
    /// windows, from the root's pools. On failure, the error names the
    /// request and the pool when `report` is set.
    ///
    /// What only fits below 4 GiB goes first, then the largest alignment:
    /// a small BAR does not take the front of a constrained 32-bit aperture
    /// and strand a large one aligned above it. What may go either way
    /// takes the below-4-GiB window first: firmware drivers reach a BAR
    /// through a mapping that does not cover above 4 GiB, and some devices
    /// that declare a 64-bit BAR still do not decode that high. The
    /// above-4-GiB window takes what does not fit.
    fn place_root(&mut self, root: usize, report: bool) -> Result<(), PciEcamError> {
        loop {
            let mut best: Option<(Request, u64, u64, (bool, u64))> = None;
            for dev_idx in 0..self.devices.len() {
                if self.devices[dev_idx].root != root || self.devices[dev_idx].parent.is_some() {
                    continue;
                }
                for (request, size, align) in self.requests_of(dev_idx) {
                    if self.placed(request) {
                        continue;
                    }
                    let rank = (self.placement(request) == Placement::Low, align);
                    if best.is_none_or(|(.., best)| rank > best) {
                        best = Some((request, size, align, rank));
                    }
                }
            }
            let Some((request, size, align, _)) = best else {
                return Ok(());
            };
            let placement = self.placement(request);
            let base = match placement {
                Placement::Io => self.take(root, Pool::Io, request, align),
                Placement::Low => self.take(root, Pool::Low, request, align),
                Placement::High => self.take(root, Pool::High, request, align),
                Placement::LowOrHigh => self
                    .take(root, Pool::Low, request, align)
                    .or_else(|| self.take(root, Pool::High, request, align)),
            };
            let Some((base, layout)) = base else {
                if report {
                    self.report_exhausted(root, request, placement, size, align);
                }
                return Err(PciEcamError::ResourceExhausted);
            };
            self.assign(request, base, layout);
        }
    }

    /// Take room for `request`, a BAR or a window of the root bus, from
    /// pool `pool` of root `root`. A window is laid out up or down,
    /// whichever leaves the larger free range in the pool, then the lower.
    fn take(
        &mut self,
        root: usize,
        pool: Pool,
        request: Request,
        align: u64,
    ) -> Option<(u64, Layout)> {
        let free = *self.roots[root].pool(pool);
        let mut taken: Option<(ResourcePool, (core::cmp::Reverse<u64>, u64), Layout)> = None;
        for layout in [Layout::Up, Layout::Down] {
            if layout == Layout::Down && matches!(request, Request::Bar(..)) {
                continue;
            }
            let (size, _, phase) = self.layout(request, layout == Layout::Down);
            let mut candidate = free;
            let Some(base) = candidate.allocate_at(size, align, phase) else {
                continue;
            };
            let rank = (core::cmp::Reverse(candidate.largest()), base);
            if taken.is_none_or(|(_, taken, _)| rank < taken) {
                taken = Some((candidate, rank, layout));
            }
        }
        let (candidate, (_, base), layout) = taken?;
        *self.roots[root].pool(pool) = candidate;
        Some((base, layout))
    }

    /// Say which request of root `root` found no room in which pool.
    fn report_exhausted(
        &self,
        root: usize,
        request: Request,
        placement: Placement,
        size: u64,
        align: u64,
    ) {
        let (dev_idx, register, what) = match request {
            Request::Bar(dev_idx, bar_idx) => {
                (dev_idx, self.devices[dev_idx].bars[bar_idx].reg, "BAR")
            }
            Request::Window(dev_idx, kind) => (dev_idx, kind.register(), "bridge window"),
        };
        let address = self.devices[dev_idx].addr;
        PciEcamError::ResourceExhausted.log_bar("resource allocation", address, register);
        fstart_log::error!(
            "pci: root bridge {} (segment={:#x} bus={:#x}) has no room in its {} pool for the {} at register {:#x}: size={:#x} alignment={:#x}",
            root,
            self.roots[root].segment,
            self.roots[root].bus_start,
            placement.pool(),
            what,
            register,
            size,
            align
        );
    }

    /// Forget where the movable BARs and the windows of root `root` went.
    fn unplace(&mut self, root: usize) {
        for device in self.devices.iter_mut().filter(|device| device.root == root) {
            for bar in device.bars.iter_mut().filter(|bar| !bar.fixed) {
                bar.allocated = false;
                bar.base = 0;
            }
            device.windows = Default::default();
        }
    }

    /// Lay out root `root` from its free `pools` with the sides decided so
    /// far: size its windows, place its root bus, then
    /// everything behind each bridge in the window placed for it. On
    /// failure, its pools are `pools` and nothing of it is placed.
    fn try_root(
        &mut self,
        root: usize,
        pools: &Pools,
        bridges: &[usize],
        report: bool,
    ) -> Result<(), PciEcamError> {
        let restore = |pci: &mut Self| {
            let root = &mut pci.roots[root];
            (root.mmio32, root.mmio64, root.io_pool) = *pools;
        };
        restore(self);
        self.unplace(root);
        let result = self.lay_out_root(root, bridges, report);
        if result.is_err() {
            restore(self);
            self.unplace(root);
        }
        result
    }

    fn lay_out_root(
        &mut self,
        root: usize,
        bridges: &[usize],
        report: bool,
    ) -> Result<(), PciEcamError> {
        self.size_windows(root, bridges)?;
        self.place_root(root, report)?;
        for &bridge in bridges {
            if self.devices[bridge].root != root {
                continue;
            }
            for kind in WINDOW_KINDS {
                let window = self.devices[bridge].windows[kind as usize];
                if window.placed {
                    let down = window.layout == Layout::Down;
                    self.lay_out(bridge, kind, window.base, true, down)?;
                }
            }
        }
        Ok(())
    }

    /// Whether BAR `bar_idx` of `dev_idx` may go above 4 GiB: a 64-bit
    /// prefetchable BAR behind bridges whose prefetchable windows are all
    /// 64-bit, of a root with memory above 4 GiB.
    fn may_go_high(&self, dev_idx: usize, bar_idx: usize) -> bool {
        let device = &self.devices[dev_idx];
        let bar = device.bars[bar_idx];
        if bar.bar_type != BarType::Memory64
            || !bar.prefetchable
            || bar.fixed
            || device.parent.is_none()
            || self.roots[device.root].mmio64.count == 0
        {
            return false;
        }
        let mut parent = device.parent;
        while let Some(bridge) = parent {
            if self.devices[bridge].prefetch != PrefetchWindow::Bits64 {
                return false;
            }
            parent = self.devices[bridge].parent;
        }
        true
    }

    /// The BAR of root `root` to try below 4 GiB next: the BAR-by-BAR
    /// allocator's order, bus by bus, then the largest first.
    fn next_untried(&self, root: usize) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, (core::cmp::Reverse<u8>, u64))> = None;
        for dev_idx in 0..self.devices.len() {
            let device = &self.devices[dev_idx];
            if device.root != root {
                continue;
            }
            for (bar_idx, bar) in device.bars.iter().enumerate() {
                if bar.side != Side::Untried {
                    continue;
                }
                let rank = (core::cmp::Reverse(device.addr.bus()), bar.size);
                if best.is_none_or(|(.., best)| rank > best) {
                    best = Some((dev_idx, bar_idx, rank));
                }
            }
        }
        best.map(|(dev_idx, bar_idx, _)| (dev_idx, bar_idx))
    }

    /// Place root `root`. Every 64-bit prefetchable BAR that may go above
    /// 4 GiB goes below if all fit; else each one that still fits there
    /// ([`Self::decide_sides`]). If no layout of subtrees fits, BAR
    /// by BAR as before ([`Self::place_bar_by_bar`]). If that does not
    /// either, the window search ([`solver`]); the error, if it finds
    /// nothing either, names the request the subtrees had no room for.
    /// Whatever is kept is checked against the root's pools as they were
    /// ([`Self::validate_plan`]). The pools are left as they were: the plan
    /// is in the devices. On failure nothing of the root is placed.
    fn allocate_root(&mut self, root: usize, bridges: &[usize]) -> Result<(), PciEcamError> {
        let Root {
            mmio32,
            mmio64,
            io_pool,
            ..
        } = self.roots[root];
        let pools = (mmio32, mmio64, io_pool);
        let (subtrees, search) = self.allocators();
        let result = if subtrees
            && ((self.decide_sides(root, &pools, bridges) && self.validate_plan(root, &pools))
                || self.place_bar_by_bar(root, &pools, bridges))
        {
            Ok(())
        } else if let Some(budget) = search {
            self.solve(root, &pools, bridges, budget).and_then(|()| {
                match self.validate_plan(root, &pools) {
                    true => Ok(()),
                    false => Err(PciEcamError::ConfigError),
                }
            })
        } else {
            Err(PciEcamError::ResourceExhausted)
        };
        if let Err(error) = result {
            self.report_failure(root, &pools, bridges, error);
            self.unplace(root);
        }
        let free = &mut self.roots[root];
        (free.mmio32, free.mmio64, free.io_pool) = pools;
        result
    }

    /// Whether the fast paths run, and the window search's budget per root
    /// if it does.
    fn allocators(&self) -> (bool, Option<solver::Budget>) {
        #[cfg(test)]
        return match self.allocators {
            Allocators::FastPaths => (true, None),
            Allocators::Search(budget) => (false, Some(budget)),
            Allocators::Both(budget) => (true, Some(budget)),
        };
        #[cfg(not(test))]
        (true, Some(solver::Budget::FIRMWARE))
    }

    /// Say why root `root` has no layout: the request the subtrees had no
    /// room for, or that the search gave up, or found one that is wrong.
    fn report_failure(
        &mut self,
        root: usize,
        pools: &Pools,
        bridges: &[usize],
        error: PciEcamError,
    ) {
        let Root {
            segment, bus_start, ..
        } = self.roots[root];
        match error {
            PciEcamError::SearchLimit => fstart_log::error!(
                "pci: root bridge {} (segment={:#x} bus={:#x}): the window search ran out of its budget",
                root,
                segment,
                bus_start
            ),
            PciEcamError::ResourceExhausted => {
                self.decide_sides(root, pools, bridges);
                if self.try_root(root, pools, bridges, true).is_ok() {
                    fstart_log::error!(
                        "pci: root bridge {} (segment={:#x} bus={:#x}): no layout forwards every BAR",
                        root,
                        segment,
                        bus_start
                    );
                }
            }
            _ => fstart_log::error!(
                "pci: root bridge {} (segment={:#x} bus={:#x}): the window search's layout is invalid",
                root,
                segment,
                bus_start
            ),
        }
    }

    /// Place root `root` as the allocator did before it laid out subtrees:
    /// BAR by BAR, first fit, bus by bus, what only fits below 4 GiB first,
    /// then the largest alignment, a BAR behind a bridge aligned to its
    /// window's granularity; each bridge's windows the spans of what is
    /// behind it. Kept only if that is a layout the hardware decodes as
    /// meant ([`Self::validate_plan`]): then whatever fitted before still
    /// does. Whether it is kept.
    fn place_bar_by_bar(&mut self, root: usize, pools: &Pools, bridges: &[usize]) -> bool {
        let restore = |pci: &mut Self| {
            let free = &mut pci.roots[root];
            (free.mmio32, free.mmio64, free.io_pool) = *pools;
            pci.unplace(root);
        };
        restore(self);
        for device in self.devices.iter_mut().filter(|device| device.root == root) {
            for bar in &mut device.bars {
                bar.side = Side::Low;
            }
        }
        for behind in [false, true] {
            while let Some((dev_idx, bar_idx)) = self.next_bar(root, behind) {
                if !self.place_bar(dev_idx, bar_idx) {
                    restore(self);
                    return false;
                }
            }
        }
        for &bridge in bridges {
            if self.devices[bridge].root == root {
                self.span_windows(bridge);
            }
        }
        let kept = self.validate_plan(root, pools);
        if !kept {
            restore(self);
        }
        kept
    }

    /// What a BAR is aligned to when placed BAR by BAR: as before, one
    /// behind a bridge to its window's granularity, so this places what it
    /// placed before. A subtree layout packs them at their own alignment.
    fn bar_alignment(&self, dev_idx: usize, bar_idx: usize) -> u64 {
        let bar = self.devices[dev_idx].bars[bar_idx];
        let behind = self.devices[dev_idx].parent.is_some();
        match bar.bar_type {
            BarType::Memory32 | BarType::Memory64 if behind => bar.size.max(0x10_0000),
            BarType::Io if behind => bar.size.max(0x1000),
            _ => bar.size,
        }
    }

    /// The unplaced movable BAR of root `root`, on its root bus or `behind`
    /// a bridge, to place next: bus by bus, what only fits below 4 GiB
    /// first, then the largest alignment.
    fn next_bar(&self, root: usize, behind: bool) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, BarRank)> = None;
        for (dev_idx, device) in self.devices.iter().enumerate() {
            if device.root != root || device.parent.is_some() != behind {
                continue;
            }
            for (bar_idx, bar) in device.bars.iter().enumerate() {
                if bar.bar_type == BarType::None || bar.fixed || bar.allocated {
                    continue;
                }
                let rank = (
                    core::cmp::Reverse(device.addr.bus()),
                    bar.bar_type == BarType::Memory32,
                    self.bar_alignment(dev_idx, bar_idx),
                );
                if best.is_none_or(|(.., best)| rank > best) {
                    best = Some((dev_idx, bar_idx, rank));
                }
            }
        }
        best.map(|(dev_idx, bar_idx, _)| (dev_idx, bar_idx))
    }

    /// Place BAR `bar_idx` of `dev_idx` first fit. A 64-bit BAR goes below
    /// 4 GiB if that leaves room for the 32-bit BARs still to come, else
    /// above, unless it is non-prefetchable behind a bridge.
    fn place_bar(&mut self, dev_idx: usize, bar_idx: usize) -> bool {
        let device = &self.devices[dev_idx];
        let (root, behind, bar) = (device.root, device.parent.is_some(), device.bars[bar_idx]);
        let align = self.bar_alignment(dev_idx, bar_idx);
        let pending: u64 = (0..self.devices.len())
            .filter(|&other| self.devices[other].root == root)
            .flat_map(|other| (0..6).map(move |index| (other, index)))
            .filter(|&(other, index)| {
                let bar = self.devices[other].bars[index];
                bar.bar_type == BarType::Memory32 && !bar.fixed && !bar.allocated
            })
            .map(|(other, index)| self.bar_alignment(other, index))
            .sum();
        let pools = &mut self.roots[root];
        let base = match bar.bar_type {
            BarType::Memory32 => pools.mmio32.allocate_aligned(bar.size, align),
            BarType::Memory64 if !bar.prefetchable && behind => {
                pools.mmio32.allocate_aligned(bar.size, align)
            }
            BarType::Memory64 => {
                let mut low = pools.mmio32;
                low.allocate_aligned(bar.size, align)
                    .filter(|_| low.free_bytes() >= pending)
                    .inspect(|_| pools.mmio32 = low)
                    .or_else(|| pools.mmio64.allocate_aligned(bar.size, align))
            }
            BarType::Io => pools.io_pool.allocate_aligned(bar.size, align),
            BarType::None => None,
        };
        let Some(base) = base else {
            return false;
        };
        let bar = &mut self.devices[dev_idx].bars[bar_idx];
        bar.base = base;
        bar.allocated = true;
        if base >= 1 << 32 {
            bar.side = Side::High;
        }
        true
    }

    /// The windows of `bridge`, from where the BARs behind it went: a
    /// window of each kind spans those that go through it, in the window's
    /// granularity.
    fn span_windows(&mut self, bridge: usize) {
        let mut spans = [(u64::MAX, 0u64); WINDOW_KINDS.len()];
        let high = self.subtree(bridge).any(|dev_idx| {
            self.devices[dev_idx]
                .bars
                .iter()
                .any(|bar| bar.side == Side::High)
        });
        let prefetch = self.devices[bridge].prefetch != PrefetchWindow::None;
        for dev_idx in self.subtree(bridge) {
            for bar in &self.devices[dev_idx].bars {
                if bar.bar_type == BarType::None || !bar.allocated {
                    continue;
                }
                let kind = match bar.bar_type {
                    BarType::Io => WindowKind::Io,
                    _ if prefetch && bar.prefetchable && (!high || bar.side == Side::High) => {
                        WindowKind::Prefetchable
                    }
                    _ => WindowKind::Memory,
                };
                let span = &mut spans[kind as usize];
                *span = (span.0.min(bar.base), span.1.max(bar.base + bar.size));
            }
        }
        for kind in WINDOW_KINDS {
            let (low, end) = spans[kind as usize];
            let window = &mut self.devices[bridge].windows[kind as usize];
            *window = Window::default();
            if low < end {
                let base = low & !(kind.granularity() - 1);
                let end = end.next_multiple_of(kind.granularity());
                window.base = base;
                window.size = end - base;
                window.placed = true;
                window.layout = Layout::Span;
                window.high = kind == WindowKind::Prefetchable && high;
            }
        }
    }

    /// The devices behind `bridge`, on any bus below it.
    fn subtree(&self, bridge: usize) -> impl Iterator<Item = usize> + '_ {
        let device = &self.devices[bridge];
        let (root, buses) = (device.root, device.secondary_bus..=device.subordinate_bus);
        (0..self.devices.len()).filter(move |&dev_idx| {
            self.devices[dev_idx].root == root && buses.contains(&self.devices[dev_idx].addr.bus())
        })
    }

    /// Decide which side of 4 GiB each 64-bit prefetchable BAR of root
    /// `root` goes to, and lay it out so. Whether it fits.
    fn decide_sides(&mut self, root: usize, pools: &Pools, bridges: &[usize]) -> bool {
        let mut candidates = false;
        for dev_idx in 0..self.devices.len() {
            if self.devices[dev_idx].root != root {
                continue;
            }
            for bar_idx in 0..6 {
                candidates |= self.may_go_high(dev_idx, bar_idx);
                self.devices[dev_idx].bars[bar_idx].side = Side::Low;
            }
        }
        if self.try_root(root, pools, bridges, false).is_ok() {
            return true;
        }
        if !candidates {
            return false;
        }
        // Else all above 4 GiB, then each below if it still fits there, in
        // the BAR-by-BAR allocator's order.
        for dev_idx in 0..self.devices.len() {
            for bar_idx in 0..6 {
                if self.devices[dev_idx].root == root && self.may_go_high(dev_idx, bar_idx) {
                    self.devices[dev_idx].bars[bar_idx].side = Side::Untried;
                }
            }
        }
        while let Some((dev_idx, bar_idx)) = self.next_untried(root) {
            self.devices[dev_idx].bars[bar_idx].side = Side::Low;
            if self.try_root(root, pools, bridges, false).is_err() {
                self.devices[dev_idx].bars[bar_idx].side = Side::High;
            }
        }
        self.try_root(root, pools, bridges, false).is_ok()
    }

    /// Allocate and program function BARs, including bridges' own BARs, and
    /// bridge forwarding windows.
    fn allocate_resources(&mut self) -> Result<(), PciEcamError> {
        // A fixed BAR behind a bridge would need the bridge's window placed
        // around it; the chipset functions that have them are on the root bus.
        for device in &self.devices {
            if let Some(bar) = device.bars.iter().find(|bar| bar.fixed)
                && device.addr.bus() != self.roots[device.root].bus_start
            {
                PciEcamError::FixedBarInvalid.log_bar(
                    "fixed BAR behind a bridge",
                    device.addr,
                    bar.reg,
                );
                return Err(PciEcamError::FixedBarInvalid);
            }
        }

        // The bridges, parents first: bus numbers are assigned depth first.
        let mut bridges: HVec<usize, MAX_PCI_DEVICES> = (0..self.devices.len())
            .filter(|&dev_idx| self.devices[dev_idx].header_type == PCI_HEADER_TYPE_BRIDGE)
            .collect();
        bridges.sort_unstable_by_key(|&dev_idx| {
            (
                self.devices[dev_idx].root,
                self.devices[dev_idx].secondary_bus,
            )
        });
        for dev_idx in 0..self.devices.len() {
            let (root, bus) = (self.devices[dev_idx].root, self.devices[dev_idx].addr.bus());
            self.devices[dev_idx].programmed = false;
            self.devices[dev_idx].parent = bridges.iter().copied().find(|&bridge| {
                bridge != dev_idx
                    && self.devices[bridge].root == root
                    && self.devices[bridge].secondary_bus == bus
            });
        }
        // A function on a bus no discovered bridge leads to has no window.
        if let Some(device) = self.devices.iter().find(|device| {
            device.parent.is_none()
                && device.addr.bus() != self.roots[device.root].bus_start
                && device
                    .bars
                    .iter()
                    .any(|bar| bar.bar_type != BarType::None && !bar.fixed)
        }) {
            PciEcamError::ConfigError.log_bar("BAR without a parent window", device.addr, PCI_BAR0);
            return Err(PciEcamError::ConfigError);
        }

        // Every root is planned and checked before anything is written.
        let before = self.roots.clone();
        for root in 0..self.roots.len() {
            if let Err(error) = self.allocate_root(root, &bridges) {
                for root in 0..root {
                    self.unplace(root);
                }
                self.roots = before;
                return Err(error);
            }
        }
        for dev_idx in 0..self.devices.len() {
            self.program(dev_idx);
        }
        Ok(())
    }

    /// Write what the allocator placed for `dev_idx`: its movable BARs with
    /// decode off while they move, a bridge's windows, then its command.
    ///
    /// I/O decode is on for a function with a placed I/O BAR, or that had
    /// it on with no movable I/O BAR (a chipset's fixed or legacy ports),
    /// and for a bridge with an open I/O window or an I/O BAR of its own;
    /// never with a movable I/O BAR unplaced, or a bridge's I/O window
    /// closed and no I/O BAR of its own.
    fn program(&mut self, dev_idx: usize) {
        let device = &self.devices[dev_idx];
        let addr = device.addr;
        let movable = |bar: &&BarInfo| bar.bar_type != BarType::None && !bar.fixed;
        if device.bars.iter().any(|bar| movable(&bar)) {
            self.write16(
                addr,
                PCI_COMMAND,
                device.original_command & !(PCI_CMD_IO | PCI_CMD_MEMORY),
            );
        }
        for bar in device
            .bars
            .iter()
            .filter(movable)
            .filter(|bar| bar.allocated)
        {
            let prefetchable = if bar.prefetchable { 0x8 } else { 0 };
            match bar.bar_type {
                BarType::Memory32 => {
                    self.write32(
                        addr,
                        bar.reg,
                        (bar.base as u32 & 0xFFFF_FFF0) | prefetchable,
                    );
                }
                BarType::Memory64 => {
                    let lo = (bar.base as u32 & 0xFFFF_FFF0) | 0x4 | prefetchable;
                    self.write32(addr, bar.reg, lo);
                    self.write32(addr, bar.reg + 4, (bar.base >> 32) as u32);
                }
                BarType::Io => self.write32(addr, bar.reg, (bar.base as u32) | 0x1),
                BarType::None => {}
            }
        }
        let bars =
            |kind: fn(BarType) -> bool| device.bars.iter().filter(move |bar| kind(bar.bar_type));
        let io = |bar_type| bar_type == BarType::Io;
        let memory = |bar_type| matches!(bar_type, BarType::Memory32 | BarType::Memory64);
        let io_placed = bars(io).all(|bar| bar.allocated);
        let has_io = bars(io).any(|bar| bar.allocated);
        let has_memory = bars(memory).any(|bar| bar.allocated);

        let mut command = device.original_command;
        if device.header_type == PCI_HEADER_TYPE_BRIDGE {
            self.program_windows(dev_idx);
            let device = &self.devices[dev_idx];
            command &= !PCI_CMD_IO;
            if io_placed && (has_io || device.windows[WindowKind::Io as usize].placed) {
                command |= PCI_CMD_IO;
            }
            // Forwarding memory is on, as it always was.
            command |= PCI_CMD_MEMORY;
        } else {
            if !io_placed {
                command &= !PCI_CMD_IO;
            } else if has_io {
                command |= PCI_CMD_IO;
            }
            if has_memory {
                command |= PCI_CMD_MEMORY;
            }
        }
        // Defer mastering when DMA ownership is explicitly left to the
        // device driver.
        if self.auto_bus_master {
            command |= PCI_CMD_BUS_MASTER;
        } else {
            command &= !PCI_CMD_BUS_MASTER;
        }
        self.write16(addr, PCI_COMMAND, command);
        self.devices[dev_idx].programmed = true;
    }

    /// Write the windows of bridge `bridge`: those placed, the others
    /// closed (base above limit).
    fn program_windows(&self, bridge: usize) {
        let device = &self.devices[bridge];
        let baddr = device.addr;
        let [io, memory, prefetchable] = device.windows;

        // Non-prefetchable memory window (base/limit in 1 MiB granularity).
        if memory.placed {
            let base_reg = ((memory.base >> 16) & 0xFFF0) as u32;
            let limit_reg = (((memory.base + memory.size - 1) >> 16) & 0xFFF0) as u32;
            self.write32(baddr, PCI_MEMORY_BASE, base_reg | (limit_reg << 16));
        } else {
            self.write32(baddr, PCI_MEMORY_BASE, 0x0000_FFFF);
        }

        // Prefetchable memory window: its upper halves only if it has them.
        let bits64 = device.prefetch == PrefetchWindow::Bits64;
        if prefetchable.placed {
            let end = prefetchable.base + prefetchable.size - 1;
            let base_reg = ((prefetchable.base >> 16) & 0xFFF0) as u32;
            let limit_reg = ((end >> 16) & 0xFFF0) as u32;
            self.write32(baddr, PCI_PREF_MEMORY_BASE, base_reg | (limit_reg << 16));
            if bits64 {
                self.write32(
                    baddr,
                    PCI_PREF_BASE_UPPER32,
                    (prefetchable.base >> 32) as u32,
                );
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, (end >> 32) as u32);
            }
        } else if device.prefetch != PrefetchWindow::None {
            self.write32(baddr, PCI_PREF_MEMORY_BASE, 0x0000_FFFF);
            if bits64 {
                self.write32(baddr, PCI_PREF_BASE_UPPER32, 0);
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, 0);
            }
        }

        if io.placed {
            let base = ((io.base >> 8) & 0xF0) as u32;
            let limit = (((io.base + io.size - 1) >> 8) & 0xF0) as u32;
            self.write32(baddr, PCI_IO_BASE, base | (limit << 8));
        } else {
            self.write32(baddr, PCI_IO_BASE, 0x00FF);
        }
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl PciEcam {
    /// Inspect config identities below explicit root-port functions using
    /// temporary bus routing. Does not probe silicon-owned root functions.
    /// Uses the same bounded traversal as resource enumeration, but
    /// never sizes/writes BARs, changes command bits or enables bus mastering.
    /// Child-first routing restoration is read back on success and error;
    /// unwinding also restores routing. Temporary BDFs are not final assignments.
    ///
    /// # Safety
    /// The caller exclusively owns an uninitialized hierarchy, with no active
    /// device transactions, and the ECAM mappings are valid. `inspect` must only
    /// read config space and must not treat temporary BDFs as stable addresses.
    pub unsafe fn with_config_topology<R>(
        &mut self,
        root_ports: &[PciAddress],
        inspect: impl FnOnce(&Self) -> R,
    ) -> Result<R, PciEcamError> {
        self.inspect_config_targets(root_ports, false, inspect)
    }

    /// Inspect only explicitly allowed functions, including functions below
    /// bridges. Unlike a subtree walk this never probes omitted function0 or
    /// reserved sibling functions. Bus assignments are temporary and must be
    /// described by the caller's validated topology.
    ///
    /// # Safety
    /// Same exclusive ownership and read-only callback contract as
    /// `with_config_topology`; every target is a valid platform-owned function.
    pub unsafe fn with_config_functions<R>(
        &mut self,
        targets: &[PciAddress],
        inspect: impl FnOnce(&Self) -> R,
    ) -> Result<R, PciEcamError> {
        self.inspect_config_targets(targets, true, inspect)
    }

    fn inspect_config_targets<R>(
        &mut self,
        targets: &[PciAddress],
        exact: bool,
        inspect: impl FnOnce(&Self) -> R,
    ) -> Result<R, PciEcamError> {
        if targets.is_empty()
            || targets.iter().any(|p| {
                !self.roots.iter().any(|r| {
                    if exact {
                        r.contains(*p)
                    } else {
                        p.segment() == r.segment && p.bus() == r.bus_start
                    }
                })
            })
        {
            return Err(PciEcamError::ConfigError);
        }
        if !self.devices.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        let mut routes = RouteJournal(HVec::new());
        let mut mode = Scan::ConfigOnly(ConfigScan {
            routes: &mut routes,
            targets,
            exact,
        });
        let result = (|| {
            for root in 0..self.roots.len() {
                self.enumerate_bus(root, self.roots[root].bus_start, &mut mode, false)?;
            }
            if exact
                && targets
                    .iter()
                    .any(|p| !self.devices.iter().any(|d| d.addr == *p))
            {
                return Err(PciEcamError::ConfigError);
            }
            Ok(inspect(self))
        })();
        let restored = routes.restore();
        self.devices.clear();
        for root in &mut self.roots {
            root.next_bus = u16::from(root.reserved_through) + 1;
        }
        restored?;
        result
    }

    /// Enumerate the PCI hierarchy, allocate BAR resources, and enable decode.
    ///
    /// On failure no BAR, window or command register has been written, but
    /// discovery has numbered the bridges' buses and left them so: treat it
    /// as fail-stop, not as a hierarchy to hand off or enumerate again.
    pub fn enumerate_and_allocate(&mut self) -> Result<(), PciEcamError> {
        self.enumerate_resource_targets(None, true)
    }

    /// Allocate resources only for explicitly selected functions and their
    /// selected bridges, without touching reserved sibling functions. Targets
    /// use the deterministic fresh bus-number allocation of this root object.
    pub fn enumerate_functions_and_allocate(
        &mut self,
        targets: &[PciAddress],
    ) -> Result<(), PciEcamError> {
        if targets.is_empty()
            || !self.devices.is_empty()
            || targets.iter().any(|p| self.root_of(*p).is_none())
        {
            return Err(PciEcamError::ConfigError);
        }
        self.enumerate_resource_targets(Some(targets), true)
    }

    /// Allocate only selected root-port subtrees, leaving unrelated host
    /// functions/retained bridges untouched. Unlike exact-function discovery,
    /// all ordinary functions below the selected ports are discovered.
    /// As with full allocation, an error can leave partial resource writes;
    /// fail-stop rather than retrying or handing off that partial topology.
    pub fn enumerate_root_ports_and_allocate(
        &mut self,
        ports: &[PciAddress],
    ) -> Result<(), PciEcamError> {
        if ports.is_empty()
            || !self.devices.is_empty()
            || ports.iter().any(|p| {
                !self
                    .roots
                    .iter()
                    .any(|r| p.segment() == r.segment && p.bus() == r.bus_start)
                    || self.device(*p).is_none_or(|d| {
                        !d.is_present() || d.read8(PCI_HEADER_TYPE) & 0x7f != PCI_HEADER_TYPE_BRIDGE
                    })
            })
        {
            return Err(PciEcamError::ConfigError);
        }
        self.enumerate_resource_targets(Some(ports), false)
    }

    fn enumerate_resource_targets(
        &mut self,
        targets: Option<&[PciAddress]>,
        exact: bool,
    ) -> Result<(), PciEcamError> {
        let mut mode = Scan::Resources { targets, exact };
        for root in 0..self.roots.len() {
            self.enumerate_bus(root, self.roots[root].bus_start, &mut mode, false)?;
        }

        if targets.is_some_and(|targets| {
            targets
                .iter()
                .any(|p| !self.devices.iter().any(|d| d.addr == *p))
        }) {
            return Err(PciEcamError::ConfigError);
        }
        if let Some(fixed) = self.fixed_bars.iter().find(|fixed| {
            !self.devices.iter().any(|dev| {
                dev.addr == fixed.address
                    && dev
                        .bars
                        .iter()
                        .any(|bar| bar.fixed && bar.reg == fixed.register)
            })
        }) {
            PciEcamError::FixedBarInvalid.log_bar(
                "fixed BAR discovery",
                fixed.address,
                fixed.register,
            );
            return Err(PciEcamError::FixedBarInvalid);
        }

        if !self.devices.is_empty() {
            self.allocate_resources()?;
        }

        Ok(())
    }

    /// Every function enumeration discovered, in scan order.
    pub fn devices(&self) -> impl Iterator<Item = PciAddress> + '_ {
        self.devices.iter().map(|device| device.addr)
    }

    /// Functions enumeration found immediately behind this bridge.
    ///
    /// This is a boot-time topology snapshot, not hot-plug presence detection:
    /// an empty bridge is not itself evidence of an installed card. Returns
    /// `None` for an undiscovered/non-bridge function or an unconfigured bus.
    /// No config-space I/O or subordinate-bus-range guessing is needed.
    pub fn bridge_devices(
        &self,
        address: PciAddress,
    ) -> Option<impl Iterator<Item = PciAddress> + '_> {
        let root = self.root_of(address)?;
        let bridge = self.devices.iter().find(|device| device.addr == address)?;
        if !matches!(
            bridge.header_type,
            PCI_HEADER_TYPE_BRIDGE | PCI_HEADER_TYPE_CARDBUS
        ) || bridge.secondary_bus == 0
            || bridge.secondary_bus == address.bus()
        {
            return None;
        }
        Some(
            self.devices
                .iter()
                .filter(move |child| {
                    child.addr.bus() == bridge.secondary_bus
                        && self.root_of(child.addr) == Some(root)
                })
                .map(|child| child.addr),
        )
    }

    /// The discovered bridge/function chain, from the root bus to `address`.
    ///
    /// Uses the allocator's cached secondary-bus topology, not ECAM rescans or
    /// subordinate-range guesses. This is suitable for ACPI `_ADR` paths and
    /// other consumers that need the actual parent chain. Unknown functions,
    /// missing/ambiguous parents and cycles fail closed. No config I/O occurs.
    pub fn device_path(&self, address: PciAddress) -> Option<HVec<PciAddress, MAX_PCI_DEVICES>> {
        if !self.devices.iter().any(|device| device.addr == address) {
            return None;
        }
        let root_index = self.root_of(address)?;
        let root_bus = self.roots[root_index].bus_start;
        let mut path = HVec::new();
        let mut current = address;
        loop {
            if path.contains(&current) {
                return None;
            }
            path.push(current).ok()?;
            if current.bus() == root_bus {
                path.reverse();
                return Some(path);
            }
            let mut parents = self.devices.iter().filter(|device| {
                matches!(
                    device.header_type,
                    PCI_HEADER_TYPE_BRIDGE | PCI_HEADER_TYPE_CARDBUS
                ) && device.secondary_bus == current.bus()
                    && self.root_of(device.addr) == Some(root_index)
            });
            let parent = parents.next()?;
            if parents.next().is_some() {
                return None;
            }
            current = parent.addr;
        }
    }

    /// The first VGA-compatible display function (class code 03_00_00h)
    /// in scan order, behind (or at) `below` if given: the function the
    /// legacy VGA ranges go to ([`Self::forward_legacy_vga`]).
    pub fn legacy_vga_display(&self, below: Option<PciAddress>) -> Option<PciAddress> {
        self.devices().find(|&address| {
            self.read32(address, crate::PCI_CLASS_REVISION) >> 8 == 0x03_0000
                && below.is_none_or(|bridge| {
                    self.device_path(address)
                        .is_some_and(|path| path.contains(&bridge))
                })
        })
    }

    /// Forward the legacy VGA ranges (memory 0xA0000–0xBFFFF, the VGA
    /// ports) from the root bus to `display`, and only there: VGA Enable
    /// off on every other bridge enumeration found, then memory and I/O
    /// decode on every function of its [`Self::device_path`] and VGA Enable
    /// on the bridges of it (PCI-to-PCI Bridge 1.2, 3.2.5.18). What decodes
    /// them above the root bus is the platform's to route, to the root
    /// bridge whose index this returns. `None` for a function enumeration
    /// did not find, or one on the path the allocator did not program
    /// ([`Self::decodes_only_what_it_placed`]); then nothing changed.
    pub fn forward_legacy_vga(&self, display: PciAddress) -> Option<usize> {
        use crate::PCI_BRIDGE_CONTROL_BITS::{DISCARD_TIMER_STATUS, VGA_ENABLE};
        let root = self.root_of(display)?;
        let path = self.device_path(display)?;
        if let Some(&address) = path
            .iter()
            .find(|&&address| !self.decodes_only_what_it_placed(address))
        {
            PciEcamError::ConfigError.log_bar("legacy VGA path", address, PCI_COMMAND);
            return None;
        }
        self.close_legacy_vga_except(&path);
        for &address in &path {
            let device = self.device(address)?;
            // SAFETY: an enumerated function; the type-0 and type-1 headers
            // share the command register.
            let header = unsafe { device.regs::<crate::PciType0Config>() };
            header.command.modify(
                crate::PCI_COMMAND_BITS::IO_SPACE::SET + crate::PCI_COMMAND_BITS::MEMORY_SPACE::SET,
            );
        }
        for (_, bridge) in self
            .type1_bridges()
            .filter(|(address, _)| path.contains(address))
        {
            bridge
                .bridge_control
                .modify(VGA_ENABLE::SET + DISCARD_TIMER_STATUS::CLEAR);
        }
        Some(root)
    }

    /// Whether `address`, with I/O and memory decode on, decodes only what
    /// the allocator placed (and, a bridge, the legacy VGA ranges once
    /// told to): it programmed its BARs, every movable one placed, and a
    /// bridge's windows, open or closed.
    fn decodes_only_what_it_placed(&self, address: PciAddress) -> bool {
        self.devices.iter().any(|device| {
            device.addr == address
                && device.programmed
                && device
                    .bars
                    .iter()
                    .all(|bar| bar.bar_type == BarType::None || bar.allocated)
        })
    }

    /// No bridge enumeration found forwards the legacy VGA ranges: there is
    /// no display for them.
    pub fn close_legacy_vga(&self) {
        self.close_legacy_vga_except(&[]);
    }

    fn close_legacy_vga_except(&self, path: &[PciAddress]) {
        use crate::PCI_BRIDGE_CONTROL_BITS::{DISCARD_TIMER_STATUS, VGA_ENABLE};
        for (_, bridge) in self
            .type1_bridges()
            .filter(|(address, _)| !path.contains(address))
        {
            // Discard Timer Status is W1C: written as 0, it stays.
            bridge
                .bridge_control
                .modify(VGA_ENABLE::CLEAR + DISCARD_TIMER_STATUS::CLEAR);
        }
    }

    /// Whether the legacy VGA ranges go to `display` and nowhere else
    /// below the root buses ([`Self::forward_legacy_vga`]).
    pub fn forwards_legacy_vga(&self, display: PciAddress) -> bool {
        use crate::PCI_BRIDGE_CONTROL_BITS::VGA_ENABLE;
        use crate::PCI_COMMAND_BITS::{IO_SPACE, MEMORY_SPACE};
        use tock_registers::interfaces::Readable;
        let Some(path) = self.device_path(display) else {
            return false;
        };
        path.iter().all(|&address| {
            self.device(address).is_some_and(|device| {
                // SAFETY: an enumerated function; read only.
                let header = unsafe { device.regs::<crate::PciType0Config>() };
                header.command.is_set(IO_SPACE) && header.command.is_set(MEMORY_SPACE)
            })
        }) && self.type1_bridges().all(|(address, bridge)| {
            bridge.bridge_control.is_set(VGA_ENABLE) == path.contains(&address)
        })
    }

    /// The type-1 bridges enumeration found, with their headers.
    fn type1_bridges(
        &self,
    ) -> impl Iterator<Item = (PciAddress, &'static crate::PciType1Config)> + '_ {
        self.devices
            .iter()
            .filter(|device| device.header_type == PCI_HEADER_TYPE_BRIDGE)
            .filter_map(|device| {
                let config = self.device(device.addr)?;
                // SAFETY: an enumerated function with a type-1 header.
                Some((device.addr, unsafe {
                    config.regs::<crate::PciType1Config>()
                }))
            })
    }

    /// Root bridge `root` (by its index in [`Self::roots`]) forwards the
    /// legacy VGA memory, which its description then reports: the platform
    /// routes the legacy VGA ranges there.
    pub fn forward_legacy_vga_memory(&mut self, root: usize) -> Result<(), PciEcamError> {
        self.described
            .get_mut(root)
            .ok_or(PciEcamError::ConfigError)?
            .forward(crate::LEGACY_VGA_MEMORY)
    }

    /// Memory BAR spans actually assigned to discovered devices, including
    /// chipset-fixed BARs. I/O BARs and unassigned/probed slots are excluded.
    pub fn assigned_memory_windows(&self) -> impl Iterator<Item = PciWindow> + '_ {
        self.assigned_memory_bars().map(|(_, window)| window)
    }

    /// Allocated prefetchable VGA apertures eligible for write combining.
    /// Non-prefetchable GPU registers and bridge windows are never included.
    pub fn prefetchable_vga_windows(&self) -> impl Iterator<Item = PciWindow> + '_ {
        self.assigned()
            .filter_map(|(address, window, prefetchable)| {
                (prefetchable
                    && window.kind == PciWindowKind::Mmio
                    && self
                        .device(address)
                        .is_some_and(|device| device.read32(0x08) >> 16 == 0x0300))
                .then_some(window)
            })
    }

    /// Assigned memory BAR spans together with their owning PCI address.
    /// Consumers can inspect the device without rediscovering BAR sizes.
    pub fn assigned_memory_bars(&self) -> impl Iterator<Item = (PciAddress, PciWindow)> + '_ {
        self.assigned_bars()
            .filter(|(_, window)| window.kind == PciWindowKind::Mmio)
    }

    /// Assigned I/O and memory BARs, using recorded sizes and live readback.
    /// No BAR sizing or configuration writes occur during inspection.
    pub fn assigned_bars(&self) -> impl Iterator<Item = (PciAddress, PciWindow)> + '_ {
        self.assigned()
            .map(|(address, window, _)| (address, window))
    }

    /// The assigned BARs, each with whether it is prefetchable.
    fn assigned(&self) -> impl Iterator<Item = (PciAddress, PciWindow, bool)> + '_ {
        self.devices.iter().flat_map(move |device| {
            device.bars.iter().filter_map(move |bar| {
                if !bar.allocated {
                    return None;
                }
                let low = self.read32(device.addr, bar.reg);
                let window = match bar.bar_type {
                    BarType::None => return None,
                    BarType::Io => PciWindow::io(u64::from(low & !3), bar.size),
                    BarType::Memory32 => PciWindow::mmio(u64::from(low & !0xf), bar.size),
                    BarType::Memory64 => PciWindow::mmio(
                        u64::from(low & !0xf)
                            | (u64::from(self.read32(device.addr, bar.reg + 4)) << 32),
                        bar.size,
                    ),
                };
                Some((device.addr, window, bar.prefetchable))
            })
        })
    }

    /// The allocator of `roots`, which it keeps: each root bridge with its
    /// own buses and the ranges it says the allocator may use. The bus
    /// ranges of a segment must not overlap.
    pub fn new(roots: impl IntoIterator<Item = PciRoot>) -> Result<Self, PciEcamError> {
        let mut pci = Self::with_pools(HVec::new());
        for described in roots {
            let root = Root::of(&described)?;
            let overlaps = pci.roots.iter().any(|other| {
                other.segment == root.segment
                    && other.bus_start <= root.bus_end
                    && root.bus_start <= other.bus_end
            });
            if overlaps || pci.roots.push(root).is_err() || pci.described.push(described).is_err() {
                return Err(PciEcamError::ConfigError);
            }
        }
        if pci.roots.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        Ok(pci)
    }

    /// The root bridges this allocator was built from, in that order: what
    /// the tables tell the OS about them comes from here.
    pub fn roots(&self) -> &[PciRoot] {
        &self.described
    }

    fn with_pools(roots: HVec<Root, MAX_PCI_ROOTS>) -> Self {
        Self {
            described: HVec::new(),
            roots,
            devices: HVec::new(),
            fixed_bars: PciFixedBars::new(),
            auto_bus_master: true,
            #[cfg(test)]
            allocators: Allocators::Both(solver::Budget::FIRMWARE),
            #[cfg(test)]
            spent: (0, 0),
        }
    }
}

impl Root {
    /// The pools of `root`: what it says the allocator may use.
    fn of(root: &PciRoot) -> Result<Self, PciEcamError> {
        let info = root.info();
        let mut mmio32 = ResourcePool::empty();
        let mut mmio64 = ResourcePool::empty();
        let mut io_pool = ResourcePool::empty();
        for range in root.allocatable() {
            match range.kind {
                PciWindowKind::Io => io_pool.push_range(range.base, range.end())?,
                // A range across 4 GiB is two: below, what a 32-bit BAR or
                // window may use; above, what only a 64-bit one may. The
                // tables still describe it as one.
                PciWindowKind::Mmio => {
                    let (base, end) = (range.base, range.end());
                    mmio32.push_range(base, end.min(plan::FOUR_GIB))?;
                    mmio64.push_range(base.max(plan::FOUR_GIB), end)?;
                }
            }
        }
        Ok(Self {
            segment: info.segment,
            ecam_base: info.ecam_base as usize,
            ecam_size: info.ecam_size() as usize,
            bus_start: info.bus_start,
            bus_end: info.bus_end,
            mmio32,
            mmio64,
            io_pool,
            reserved_through: info.bus_start,
            next_bus: u16::from(info.bus_start) + 1,
        })
    }
}

/// The ECAM accessors describe the first root bridge: all of it on a
/// single-root platform.
impl PciEcam {
    pub fn ecam_base(&self) -> u64 {
        self.roots[0].ecam_base as u64
    }

    pub fn ecam_size(&self) -> u64 {
        self.roots[0].ecam_size as u64
    }

    pub fn bus_start(&self) -> u8 {
        self.roots[0].bus_start
    }

    pub fn bus_end(&self) -> u8 {
        self.roots[0].bus_end
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
}

#[allow(unused_unsafe)]
impl ConfigRegionAccess for PciEcam {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        // SAFETY: the caller guarantees that the PCI address and offset are valid.
        unsafe { self.read32(address, offset) }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        // SAFETY: the caller guarantees that the PCI address and offset are valid.
        unsafe { self.write32(address, offset, value) }
    }
}

#[path = "ecam_host/plan.rs"]
mod plan;
#[path = "ecam_host/solver.rs"]
mod solver;

#[cfg(test)]
#[path = "ecam_host/allocator_tests.rs"]
mod allocator_tests;

#[cfg(test)]
#[path = "ecam_host/master_tests.rs"]
mod master_tests;

#[cfg(test)]
#[path = "ecam_host/solver_tests.rs"]
mod solver_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PciRole::{Forwarded, Kept, Own, Window};
    use crate::PciRootInfo;
    use alloc::vec;
    use alloc::vec::Vec;

    pub(super) fn test_pci() -> (PciEcam, Vec<u32>) {
        test_pci_with_buses(3)
    }

    pub(super) fn test_pci_with_buses(buses: usize) -> (PciEcam, Vec<u32>) {
        let mut config_space = vec![0u32; buses << 18];
        let ecam_base = config_space.as_mut_ptr() as usize;
        let root = Root {
            segment: 0,
            ecam_base,
            ecam_size: buses << 20,
            bus_start: 0,
            bus_end: buses as u8 - 1,
            mmio32: ResourcePool::new(0x8000_0000, 0x1000_0000).unwrap(),
            mmio64: ResourcePool::empty(),
            io_pool: ResourcePool::new(0x1000, 0xf000).unwrap(),
            reserved_through: 0,
            next_bus: 1,
        };
        (PciEcam::with_pools(HVec::from_iter([root])), config_space)
    }

    fn config_topology_fixture() -> (PciEcam, Vec<u32>) {
        let (pci, mut ram) = test_pci();
        ram.fill(u32::MAX);
        for (addr, id, header, class) in [
            // Unselected silicon host functions must not be scanned.
            (PciAddress::new(0, 0, 0, 0), 0x14a41022, 0x80, 0x06000000),
            (PciAddress::new(0, 0, 0, 1), 0xdeadc0de, 1, 0x06040000),
            (PciAddress::new(0, 0, 1, 0), 0x14781002, 1, 0x06040000),
            (PciAddress::new(0, 1, 0, 0), 0x14791002, 1, 0x06040000),
            (PciAddress::new(0, 2, 0, 0), 0x744c1002, 0x80, 0x03000000),
            (PciAddress::new(0, 2, 0, 1), 0x01001d82, 0, 0x01080200),
        ] {
            let dev = pci.device(addr).unwrap();
            dev.write32(PCI_VENDOR_ID, id);
            dev.write8(PCI_HEADER_TYPE, header);
            dev.write32(PCI_COMMAND, 0xa5a50003);
            dev.write32(crate::PCI_CLASS_REVISION, class);
        }
        pci.write32(PciAddress::new(0, 0, 1, 0), PCI_PRIMARY_BUS, 0x7a000000);
        pci.write32(PciAddress::new(0, 1, 0, 0), PCI_PRIMARY_BUS, 0x6b000000);
        (pci, ram)
    }

    #[test]
    fn a_32_gib_bar_is_one_64_bit_bar_not_a_32_bit_upper_half() {
        // AMD 1002:7460 BAR0: 32 GiB, 64-bit prefetchable.
        assert!(!bar_unimplemented(0x0000_000c, 0));
        assert_eq!(
            size_from_moving_bits(0xffff_fff8_0000_0000, 0xFFFF_FFFF_FFFF_FFF0),
            Ok(0x8_0000_0000)
        );
        // Unimplemented and fixed 32-bit registers stay absent.
        assert!(bar_unimplemented(0, 0));
        assert!(bar_unimplemented(0x0000_0008, 0));
        assert!(!bar_unimplemented(0xffff_c000, 0xffff_c000));
    }

    #[test]
    fn selected_resource_subtree_preserves_owned_bus_prefix_and_defers_dma() {
        let (mut pci, mut ram) = test_pci();
        ram.fill(u32::MAX);
        let retained = PciAddress::new(0, 0, 2, 0);
        let selected = PciAddress::new(0, 0, 1, 1); // no function zero
        let child = PciAddress::new(0, 2, 0, 0);
        for (address, header, class) in [
            (retained, 1, 0x06040000),
            (selected, 1, 0x06040000),
            (child, 0, 0x01080200),
        ] {
            let dev = pci.device(address).unwrap();
            dev.write32(PCI_VENDOR_ID, 0x12341022);
            dev.write8(PCI_HEADER_TYPE, header);
            dev.write32(crate::PCI_CLASS_REVISION, class);
            dev.write32(PCI_COMMAND, 0);
            for index in 0..if header == 1 { 2 } else { 6 } {
                dev.write32(PCI_BAR0 + index * 4, 0);
            }
        }
        pci.write32(retained, PCI_PRIMARY_BUS, 0x00010100);
        pci.write32(selected, PCI_PRIMARY_BUS, 0x7a000000);
        pci.write16(child, PCI_COMMAND, PCI_CMD_BUS_MASTER);
        let before = ram.clone();
        assert_eq!(
            pci.reserve_bus_prefix(1, 0, 1),
            Err(PciEcamError::ConfigError)
        );
        assert_eq!(
            pci.reserve_bus_prefix(0, 0, 3),
            Err(PciEcamError::ConfigError)
        );
        pci.reserve_bus_prefix(0, 0, 1).unwrap();
        pci.disable_auto_bus_master().unwrap();
        // SAFETY: modeled ECAM and read-only callback; journal must retain
        // the allocator's reservation after temporary discovery.
        assert_eq!(
            unsafe { pci.with_config_topology(&[selected], |p| p.devices().count()) }.unwrap(),
            2
        );
        assert_eq!(ram, before);
        assert_eq!(pci.roots[0].next_bus, 2);
        assert_eq!(
            pci.enumerate_root_ports_and_allocate(&[child]),
            Err(PciEcamError::ConfigError)
        );
        assert_eq!(ram, before);
        pci.enumerate_root_ports_and_allocate(&[selected]).unwrap();
        assert_eq!(pci.devices().count(), 2);
        assert_eq!(pci.read32(selected, PCI_PRIMARY_BUS), 0x7a020200);
        assert_eq!(pci.read32(retained, PCI_PRIMARY_BUS), 0x00010100);
        assert_eq!(&ram[1 << 18..2 << 18], &before[1 << 18..2 << 18]);
        for address in [selected, child] {
            assert_eq!(pci.read16(address, PCI_COMMAND) & PCI_CMD_BUS_MASTER, 0);
        }
        assert_eq!(
            pci.reserve_bus_prefix(0, 0, 2),
            Err(PciEcamError::ConfigError)
        );
        assert_eq!(
            pci.disable_auto_bus_master(),
            Err(PciEcamError::ConfigError)
        );
    }

    #[test]
    fn exhausted_retained_prefix_survives_failed_config_scope_without_wraparound() {
        let (mut pci, ram) = config_topology_fixture();
        let before = ram.clone();
        pci.reserve_bus_prefix(0, 0, 2).unwrap();
        pci.reserve_bus_prefix(0, 0, 1).unwrap(); // Cannot shrink ownership.
        // SAFETY: modeled ECAM; no downstream bus remains to assign.
        assert_eq!(
            unsafe { pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |_| ()) },
            Err(PciEcamError::ResourceExhausted)
        );
        assert_eq!(ram, before);
        assert_eq!(pci.roots[0].next_bus, 3);
        assert_eq!(pci.roots[0].reserved_through, 2);
    }

    #[test]
    fn config_only_walks_nested_multifunction_tree_and_restores_every_byte() {
        let (mut pci, ram) = config_topology_fixture();
        let before = ram.clone();
        let found = unsafe {
            pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |view| {
                assert_eq!(
                    view.read32(PciAddress::new(0, 0, 1, 0), PCI_PRIMARY_BUS),
                    0x7a020100
                );
                assert_eq!(
                    view.read32(PciAddress::new(0, 1, 0, 0), PCI_PRIMARY_BUS),
                    0x6b020201
                );
                assert!(
                    view.devices
                        .iter()
                        .all(|d| d.bars.iter().all(|b| b.bar_type == BarType::None))
                );
                view.devices().collect::<Vec<_>>()
            })
        }
        .unwrap();
        assert_eq!(found.len(), 4);
        assert!(found.contains(&PciAddress::new(0, 2, 0, 1)));
        assert_eq!(ram, before);
        assert!(pci.devices.is_empty());
        assert_eq!(pci.roots[0].next_bus, 1);
        assert_eq!(
            unsafe {
                pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |p| p.devices().count())
            }
            .unwrap(),
            4
        );
        assert_eq!(ram, before);
    }

    #[test]
    fn config_only_selected_root_port_does_not_require_function_zero() {
        let (mut pci, ram) = config_topology_fixture();
        let original = PciAddress::new(0, 0, 1, 0);
        let selected = PciAddress::new(0, 0, 1, 1);
        for reg in (0..256u16).step_by(4) {
            pci.write32(selected, reg, pci.read32(original, reg));
        }
        pci.write32(original, PCI_VENDOR_ID, PCI_VENDOR_INVALID);
        let before = ram.clone();
        assert_eq!(
            unsafe { pci.with_config_topology(&[selected], |p| p.devices().count()) }.unwrap(),
            4
        );
        assert_eq!(ram, before);
        assert_eq!(
            unsafe { pci.with_config_topology(&[], |_| ()) },
            Err(PciEcamError::ConfigError)
        );
    }

    #[test]
    fn pcie_downstream_bus_never_probes_nonzero_device_numbers() {
        let (mut pci, ram) = config_topology_fixture();
        let downstream = pci.device(PciAddress::new(0, 1, 0, 0)).unwrap();
        downstream.write16(crate::PCI_STATUS, 0x10);
        downstream.write8(crate::PCI_CAPABILITIES_PTR, 0x40);
        downstream.write16(0x40, 0x10);
        downstream.write16(0x42, 0x60);
        let impossible = PciAddress::new(0, 2, 1, 0);
        pci.write32(impossible, PCI_VENDOR_ID, 0xdeadbeef);
        pci.device(impossible).unwrap().write8(PCI_HEADER_TYPE, 1);
        let before = ram.clone();
        assert_eq!(
            unsafe {
                pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |p| p.devices().count())
            }
            .unwrap(),
            4
        );
        assert_eq!(ram, before);
    }

    fn exact_function_fixture() -> (PciEcam, Vec<u32>, [PciAddress; 2]) {
        let (pci, ram) = config_topology_fixture();
        let bridge = PciAddress::new(0, 0, 1, 0);
        let endpoint = PciAddress::new(0, 1, 0, 4);
        let dev = pci.device(endpoint).unwrap();
        dev.write32(PCI_VENDOR_ID, 0x14c91022);
        dev.write8(PCI_HEADER_TYPE, 0);
        dev.write32(crate::PCI_CLASS_REVISION, 0x0c033000);
        dev.write16(PCI_COMMAND, 0);
        for reg in (PCI_BAR0..=PCI_BAR0 + 20).step_by(4) {
            dev.write32(reg, 0);
        }
        for reg in [PCI_BAR0, PCI_BAR0 + 4] {
            pci.write32(bridge, reg, 0);
        }
        (pci, ram, [bridge, endpoint])
    }

    #[test]
    fn exact_config_scope_skips_hidden_function_zero_and_restores_all_bytes() {
        let (mut pci, ram, targets) = exact_function_fixture();
        let before = ram.clone();
        let found =
            unsafe { pci.with_config_functions(&targets, |p| p.devices().collect::<Vec<_>>()) }
                .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.contains(&targets[1]));
        assert_eq!(ram, before);
        let absent = [targets[0], PciAddress::new(0, 1, 0, 7)];
        assert_eq!(
            unsafe { pci.with_config_functions(&absent, |_| ()) },
            Err(PciEcamError::ConfigError)
        );
        assert_eq!(ram, before);
    }
    #[test]
    fn exact_resource_scope_allocates_selected_endpoint_without_touching_siblings() {
        let (mut pci, _ram, targets) = exact_function_fixture();
        let hidden = PciAddress::new(0, 1, 0, 0);
        let original = pci.read32(hidden, PCI_PRIMARY_BUS);
        pci.enumerate_functions_and_allocate(&targets).unwrap();
        assert_eq!(pci.devices().count(), 2);
        assert_ne!(pci.read32(targets[1], PCI_BAR0), 0);
        assert_ne!(pci.read16(targets[1], PCI_COMMAND) & PCI_CMD_BUS_MASTER, 0);
        assert_eq!(pci.read32(hidden, PCI_PRIMARY_BUS), original);
    }

    #[test]
    fn config_only_exhaustion_restores_routes_and_resets_metadata() {
        let (mut pci, ram) = config_topology_fixture();
        let before = ram.clone();
        pci.roots[0].bus_end = 1;
        for _ in 0..2 {
            let result = unsafe {
                pci.with_config_topology::<()>(&[PciAddress::new(0, 0, 1, 0)], |_| {
                    panic!("incomplete walk must not be inspected")
                })
            };
            assert_eq!(result, Err(PciEcamError::ResourceExhausted));
            assert_eq!(ram, before);
            assert!(pci.devices.is_empty());
            assert_eq!(pci.roots[0].next_bus, 1);
        }
    }

    #[test]
    fn config_only_unwinding_restores_the_hierarchy() {
        extern crate std;
        let (mut pci, ram) = config_topology_fixture();
        let before = ram.clone();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), _> = unsafe {
                pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |_| {
                    panic!("observer failed")
                })
            };
        }));
        assert!(panic.is_err());
        assert_eq!(ram, before);
    }

    #[test]
    fn route_journal_restores_children_before_parents() {
        let mut ram = [0u32; 16];
        let parent = crate::EcamDevice::at_config_base(
            PciAddress::new(0, 0, 1, 0),
            ram.as_mut_ptr() as usize,
        );
        // Aliased test registers make restoration order directly observable.
        let child = crate::EcamDevice::at_config_base(
            PciAddress::new(0, 1, 0, 0),
            ram.as_mut_ptr() as usize,
        );
        let mut routes = RouteJournal(HVec::from_iter([(parent, 0xaabbccdd), (child, 0x11223344)]));
        routes.restore().unwrap();
        assert_eq!(parent.read32(PCI_PRIMARY_BUS), 0xaabbccdd);
    }

    #[test]
    fn subwidth_accesses_preserve_neighboring_config_bytes() {
        let (pci, _memory) = test_pci();
        let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        for lane in 0..4 {
            device.write32(0x40, 0xaabb_ccdd);
            device.write8(0x40 + lane, 0x12);
            let shift = lane * 8;
            assert_eq!(device.read8(0x40 + lane), 0x12);
            assert_eq!(
                device.read32(0x40),
                (0xaabb_ccdd & !(0xff << shift)) | (0x12 << shift)
            );
        }
        device.write32(0x40, 0xaabb_ccdd);
        device.write16(0x42, 0x1234);
        assert_eq!(device.read16(0x42), 0x1234);
        assert_eq!(device.read32(0x40), 0x1234_ccdd);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn unaligned_loads_and_stores_use_exact_byte_spans() {
        let (pci, _memory) = test_pci();
        let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        for offset in 1..4u16 {
            device.write32(0x40, 0xa5a5_a5a5);
            device.write32(0x44, 0xa5a5_a5a5);
            // SAFETY: the entire access is backed by ordinary test RAM.
            unsafe {
                assert_eq!(
                    device.try_write32_unaligned(0x40 + offset, 0x1234_5678),
                    Some(())
                );
                assert_eq!(
                    device.try_read32_unaligned(0x40 + offset),
                    Some(0x1234_5678)
                );
            }
            let mut expected = [0xa5; 8];
            let start = usize::from(offset);
            expected[start..start + 4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            let observed: [u8; 8] = core::array::from_fn(|i| device.read8(0x40 + i as u16));
            assert_eq!(observed, expected);
            assert_eq!(device.try_read32(0x40 + offset), None);
            assert_eq!(device.try_write32(0x40 + offset, 0), None);

            device.write32(0x40, 0xa5a5_a5a5);
            device.write32(0x44, 0xa5a5_a5a5);
            // SAFETY: the entire access is backed by ordinary test RAM.
            unsafe {
                assert_eq!(
                    device.try_write16_unaligned(0x40 + offset, 0xcdef),
                    Some(())
                );
                assert_eq!(device.try_read16_unaligned(0x40 + offset), Some(0xcdef));
            }
            let mut expected = [0xa5; 8];
            expected[start..start + 2].copy_from_slice(&0xcdefu16.to_le_bytes());
            let observed: [u8; 8] = core::array::from_fn(|i| device.read8(0x40 + i as u16));
            assert_eq!(observed, expected);
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn unaligned_config_spans_cannot_cross_function_boundary() {
        let (pci, _memory) = test_pci();
        let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        device.write32(0xff8, 0xa5a5_a5a5);
        device.write32(0xffc, 0xa5a5_a5a5);
        // SAFETY: the valid span is test RAM; invalid spans must be rejected
        // before any memory access, including the unmapped segment below.
        unsafe {
            assert_eq!(device.try_write32_unaligned(0xffb, 0x1234_5678), Some(()));
            assert_eq!(device.try_read32_unaligned(0xffb), Some(0x1234_5678));
            assert_eq!(device.try_read16_unaligned(0xfff), None);
            assert_eq!(device.try_write16_unaligned(0xfff, 0), None);
            assert_eq!(device.try_read32_unaligned(0xffd), None);
            assert_eq!(device.try_write32_unaligned(0xffd, 0), None);
            assert_eq!(device.try_read32_unaligned(u16::MAX), None);
            let unmapped = crate::EcamDevice::new_address(PciAddress::new(1, 0, 0, 0));
            assert_eq!(unmapped.try_read16_unaligned(1), None);
            assert_eq!(unmapped.try_write16_unaligned(1, 0), None);
            assert_eq!(unmapped.try_read32_unaligned(1), None);
            assert_eq!(unmapped.try_write32_unaligned(1, 0), None);
        }
        assert_eq!(device.read32(0xff8), 0x78a5_a5a5);
        assert_eq!(device.read32(0xffc), 0xa512_3456);
    }

    #[test]
    fn invalid_accesses_are_not_absent_device_responses() {
        let (mut pci, _memory) = test_pci();
        assert!(pci.device(PciAddress::new(1, 0, 0, 0)).is_none());
        assert!(pci.device(PciAddress::new(0, 3, 0, 0)).is_none());
        let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        device.write32(0xffc, u32::MAX);
        assert_eq!(device.try_read8(0xfff), Some(u8::MAX));
        assert_eq!(device.try_read8(0x1000), None);
        assert_eq!(device.try_read16(0xfff), None);
        assert_eq!(device.try_write32(0xffe, 0), None);
        assert_eq!(device.try_write8(0x1000, 0), None);
        assert_eq!(device.read32(0xffc), u32::MAX);
        pci.roots[0].ecam_size = 0xfff; // A truncated function is not a mapped device.
        assert!(pci.device(PciAddress::new(0, 0, 0, 0)).is_none());
    }

    #[test]
    fn bridge_children_distinguish_empty_slots_from_present_ports() {
        let (mut pci, _memory) = test_pci();
        let bridge = PciAddress::new(0, 0, 28, 3);
        assert!(pci.bridge_devices(bridge).is_none());
        pci.devices
            .push(PciDev {
                header_type: PCI_HEADER_TYPE_BRIDGE,
                secondary_bus: 1,
                subordinate_bus: 2,
                ..endpoint(bridge, &[])
            })
            .ok()
            .unwrap();
        assert_eq!(pci.bridge_devices(bridge).unwrap().count(), 0);
        // A sibling's bus or another segment must not occupy this slot.
        pci.devices
            .push(endpoint(PciAddress::new(0, 2, 0, 0), &[]))
            .ok()
            .unwrap();
        pci.devices
            .push(endpoint(PciAddress::new(1, 1, 0, 0), &[]))
            .ok()
            .unwrap();
        assert_eq!(pci.bridge_devices(bridge).unwrap().count(), 0);
        let card = PciAddress::new(0, 1, 0, 0);
        pci.devices.push(endpoint(card, &[])).ok().unwrap();
        assert_eq!(
            pci.bridge_devices(bridge).unwrap().collect::<Vec<_>>(),
            [card]
        );
        assert!(pci.bridge_devices(card).is_none());
        pci.devices[0].secondary_bus = 0;
        assert!(pci.bridge_devices(bridge).is_none());
    }

    #[test]
    fn device_paths_follow_secondary_buses_and_reject_invalid_topology() {
        let (mut pci, _memory) = test_pci();
        let root_bridge = PciAddress::new(0, 0, 8, 1);
        let child_bridge = PciAddress::new(0, 1, 3, 2);
        let gpu = PciAddress::new(0, 2, 0, 1);
        for (address, secondary) in [(root_bridge, 1), (child_bridge, 2)] {
            assert!(
                pci.devices
                    .push(PciDev {
                        header_type: PCI_HEADER_TYPE_BRIDGE,
                        secondary_bus: secondary,
                        subordinate_bus: 2,
                        ..endpoint(address, &[])
                    })
                    .is_ok()
            );
        }
        assert!(pci.devices.push(endpoint(gpu, &[])).is_ok());
        assert_eq!(
            pci.device_path(gpu).unwrap().as_slice(),
            &[root_bridge, child_bridge, gpu]
        );
        assert_eq!(
            pci.device_path(root_bridge).unwrap().as_slice(),
            &[root_bridge]
        );
        assert!(pci.device_path(PciAddress::new(0, 2, 1, 0)).is_none());
        pci.devices[0].secondary_bus = 2; // Ambiguous parent of the GPU.
        assert!(pci.device_path(gpu).is_none());
        pci.devices[0].secondary_bus = 1;
        pci.devices[1].secondary_bus = 0; // Missing parent.
        assert!(pci.device_path(gpu).is_none());
        pci.devices[1].secondary_bus = 2;
        pci.devices[0].addr = PciAddress::new(0, 2, 8, 1); // Cycle: 2 -> 1 -> 2.
        assert!(pci.device_path(gpu).is_none());
    }

    /// A root port with a bridge to a VGA-compatible display behind it, an
    /// accelerator (display, not VGA-compatible), a second VGA display on
    /// the root bus and a second root port that already forwards the legacy
    /// VGA ranges (a previous owner's choice).
    #[test]
    fn legacy_vga_goes_to_the_display_along_its_path() {
        let (mut pci, mut ram) = test_pci();
        ram.fill(0);
        let port = PciAddress::new(0, 0, 1, 1);
        let bridge = PciAddress::new(0, 1, 0, 0);
        let bmc = PciAddress::new(0, 2, 0, 0);
        let accelerator = PciAddress::new(0, 0, 2, 0);
        let gpu = PciAddress::new(0, 0, 3, 0);
        let other = PciAddress::new(0, 0, 4, 1);
        for (address, header, class, secondary, control) in [
            (accelerator, 0, 0x0380_0000, 0, 0),
            (port, 1, 0x0604_0000, 1, 0x0400),
            (bridge, 1, 0x0604_0000, 2, 0x0400),
            (bmc, 0, 0x0300_0041, 0, 0),
            (gpu, 0, 0x0300_00c1, 0, 0),
            // VGA Enable and VGA 16-bit decode on, Discard Timer Status
            // (W1C) pending.
            (other, 1, 0x0604_0000, 3, 0x0418),
        ] {
            let dev = pci.device(address).unwrap();
            dev.write8(PCI_HEADER_TYPE, header);
            dev.write32(crate::PCI_CLASS_REVISION, class);
            dev.write16(0x3e, control);
            assert!(
                pci.devices
                    .push(PciDev {
                        header_type: header,
                        secondary_bus: secondary,
                        subordinate_bus: secondary.max(2),
                        programmed: true,
                        ..endpoint(address, &[])
                    })
                    .is_ok()
            );
        }
        // The first VGA-compatible one in scan order, or the one behind
        // the port the board names.
        assert_eq!(pci.legacy_vga_display(None), Some(bmc));
        assert_eq!(pci.legacy_vga_display(Some(port)), Some(bmc));
        assert_eq!(pci.legacy_vga_display(Some(gpu)), Some(gpu));
        assert_eq!(pci.legacy_vga_display(Some(accelerator)), None);
        assert!(!pci.forwards_legacy_vga(bmc));
        assert_eq!(pci.forward_legacy_vga(bmc), Some(0));
        assert!(pci.forwards_legacy_vga(bmc));
        assert!(!pci.forwards_legacy_vga(gpu));
        for address in [port, bridge, bmc] {
            assert_eq!(pci.read16(address, PCI_COMMAND) & 3, 3);
        }
        // VGA Enable on the bridges; their discard timer status (W1C)
        // written as 0.
        for address in [port, bridge] {
            assert_eq!(pci.read16(address, 0x3e), 1 << 3);
        }
        // Off the path: VGA Enable cleared, the 16-bit decode kept, the
        // W1C status written as 0.
        assert_eq!(pci.read16(other, 0x3e), 1 << 4);
        for address in [accelerator, gpu] {
            assert_eq!(pci.read16(address, PCI_COMMAND), 0);
        }
        // Moving the legacy VGA to the root-bus display closes the old path.
        assert_eq!(pci.forward_legacy_vga(gpu), Some(0));
        assert!(pci.forwards_legacy_vga(gpu));
        for address in [port, bridge, other] {
            assert_eq!(pci.read16(address, 0x3e) & (1 << 3), 0);
        }
        assert_eq!(pci.forward_legacy_vga(PciAddress::new(0, 1, 5, 0)), None);
        assert!(pci.forwards_legacy_vga(gpu));
        pci.close_legacy_vga();
        assert!(!pci.forwards_legacy_vga(bmc));
        pci.write16(other, 0x3e, 0x0408);
        pci.close_legacy_vga();
        assert_eq!(pci.read16(other, 0x3e), 0);
    }

    /// The legacy VGA ports go only through functions whose decode the
    /// allocator set up: none before allocation, none while a function on
    /// the path has an I/O BAR it did not place; then nothing changes.
    #[test]
    fn legacy_vga_needs_a_path_the_allocator_programmed() {
        let (mut pci, _config_space) = test_pci();
        let port = PciAddress::new(0, 0, 1, 0);
        let display = PciAddress::new(0, 1, 0, 0);
        pci.write32(display, crate::PCI_CLASS_REVISION, 0x0300_0000);
        for device in [
            bridge_at(port, 1, 1),
            endpoint(
                display,
                &[(0, BarType::Memory32, 0x100_0000), (2, BarType::Io, 0x80)],
            ),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }
        let decode = |pci: &PciEcam, address| pci.read16(address, PCI_COMMAND) & 3;

        assert_eq!(pci.forward_legacy_vga(display), None);
        assert_eq!((decode(&pci, port), decode(&pci, display)), (0, 0));
        assert_eq!(pci.read16(port, 0x3e), 0);

        pci.allocate_resources().unwrap();
        pci.write16(display, PCI_COMMAND, 0);
        pci.devices[1].bars[2].allocated = false;
        assert_eq!(pci.forward_legacy_vga(display), None);
        assert_eq!(decode(&pci, display), 0);
        assert_eq!(pci.read16(port, 0x3e), 0);

        pci.devices[1].bars[2].allocated = true;
        assert_eq!(pci.forward_legacy_vga(display), Some(0));
        assert_eq!(decode(&pci, display), 3);
        assert_eq!(decode(&pci, port), 3);
        assert!(pci.forwards_legacy_vga(display));
    }

    /// The root the platform routes the legacy VGA memory to reports it.
    #[test]
    fn the_legacy_vga_root_forwards_the_vga_memory() {
        let info = |bus_start, bus_end| PciRootInfo {
            segment: 0,
            ecam_base: 0xe000_0000,
            bus_start,
            bus_end,
        };
        let root = |bus_start, bus_end, base| {
            PciRoot::new(
                info(bus_start, bus_end),
                [
                    Window.mmio(base, 0x1000_0000),
                    Forwarded.mmio(0xc_0000, 0x4_0000),
                ],
            )
            .unwrap()
        };
        let mut pci =
            PciEcam::new([root(0, 0x3f, 0xc000_0000), root(0x40, 0x7f, 0xd000_0000)]).unwrap();
        pci.forward_legacy_vga_memory(1).unwrap();
        assert!(
            pci.roots()[0]
                .mmio()
                .all(|range| *range.start() != 0xa_0000)
        );
        // Joined with the option ROM range it touches.
        assert!(
            pci.roots()[1]
                .mmio()
                .any(|range| range == (0xa_0000..=0xf_ffff))
        );
        assert!(pci.forward_legacy_vga_memory(2).is_err());
    }

    #[test]
    fn sibling_bridge_windows_do_not_interleave() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0xf800_0000, 0x0700_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let bridge = |device, bus| PciDev {
            header_type: PCI_HEADER_TYPE_BRIDGE,
            secondary_bus: bus,
            subordinate_bus: bus,
            ..endpoint(PciAddress::new(0, 0, device, 1), &[])
        };
        // Zeppelin: xHCI's 64-bit BAR behind 7.1, SATA's 32-bit BAR behind 8.1.
        let xhci = PciAddress::new(0, 1, 0, 3);
        let sata = PciAddress::new(0, 2, 0, 2);
        for device in [
            bridge(7, 1),
            bridge(8, 2),
            endpoint(xhci, &[(0, BarType::Memory64, 0x10_0000)]),
            endpoint(sata, &[(5, BarType::Memory32, 0x1000)]),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        let window = |device| {
            let value = pci.read32(PciAddress::new(0, 0, device, 1), PCI_MEMORY_BASE);
            (value & 0xfff0) << 16..=(value | 0xf_ffff)
        };
        let (xhci_window, sata_window) = (window(7), window(8));
        assert!(xhci_window.end() < sata_window.start());
        assert!(xhci_window.contains(&(pci.read32(xhci, PCI_BAR0) & !0xf)));
        assert!(sata_window.contains(&(pci.read32(sata, PCI_BAR0 + 20) & !0xf)));
    }

    /// The non-prefetchable memory window of `bridge`, inclusive.
    pub(super) fn memory_window(
        pci: &PciEcam,
        bridge: PciAddress,
    ) -> core::ops::RangeInclusive<u64> {
        let value = pci.read32(bridge, PCI_MEMORY_BASE);
        u64::from((value & 0xfff0) << 16)..=u64::from(value | 0xf_ffff)
    }

    /// Every bridge's memory window holds its subtree's memory BARs and no
    /// other BAR, fixed or movable.
    fn assert_windows_hold_only_their_subtrees(pci: &PciEcam) {
        for bridge in pci
            .devices
            .iter()
            .filter(|d| d.header_type == PCI_HEADER_TYPE_BRIDGE)
        {
            let window = memory_window(pci, bridge.addr);
            let subtree = bridge.secondary_bus..=bridge.subordinate_bus;
            for (address, bar) in pci.assigned_memory_bars() {
                let start = bar.base;
                let end = bar.base + bar.size - 1;
                if subtree.contains(&address.bus()) {
                    assert!(window.contains(&start) && window.contains(&end));
                } else {
                    assert!(
                        end < *window.start() || start > *window.end(),
                        "{address:?}'s BAR at {start:#x} is in {:?}'s window {window:x?}",
                        bridge.addr
                    );
                }
            }
        }
    }

    pub(super) fn bridge_at(address: PciAddress, secondary: u8, subordinate: u8) -> PciDev {
        PciDev {
            header_type: PCI_HEADER_TYPE_BRIDGE,
            secondary_bus: secondary,
            subordinate_bus: subordinate,
            ..endpoint(address, &[])
        }
    }

    /// The S8040's first IOS: a root port with the AST2600 behind it (a
    /// 16 MiB framebuffer, aligned up past the pool's front, and a 256 KiB
    /// register BAR), then the internal bridge with the xHCI. Placed BAR by
    /// BAR, the xHCI went into the gap below the framebuffer, inside the
    /// AST's root port window. Now the AST's window is placed whole, laid
    /// out down (the register BAR below the framebuffer, the window ending
    /// at a 16 MiB boundary), and the xHCI's window takes the gap below.
    #[test]
    fn a_later_bus_does_not_fill_an_earlier_bridges_alignment_gap() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0xc880_0000, 0x0700_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let ast = PciAddress::new(0, 1, 0, 0);
        let xhci = PciAddress::new(0, 2, 0, 4);
        for device in [
            bridge_at(PciAddress::new(0, 0, 5, 3), 1, 1),
            endpoint(
                ast,
                &[
                    (0, BarType::Memory32, 0x100_0000),
                    (1, BarType::Memory32, 0x4_0000),
                ],
            ),
            bridge_at(PciAddress::new(0, 0, 7, 1), 2, 2),
            endpoint(xhci, &[(0, BarType::Memory64, 0x10_0000)]),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(ast, PCI_BAR0) & !0xf, 0xc900_0000);
        assert_eq!(pci.read32(ast, crate::PCI_BAR1) & !0xf, 0xc8fc_0000);
        assert_eq!(pci.read32(xhci, PCI_BAR0) & !0xf, 0xc880_0000);
        assert_eq!(
            memory_window(&pci, PciAddress::new(0, 0, 5, 3)),
            0xc8f0_0000..=0xc9ff_ffff
        );
        assert_windows_hold_only_their_subtrees(&pci);
    }

    /// A root-bus BAR aligned to the end of the pool leaves the gap below
    /// it, where a bridge's window still fits. Every BAR above all the
    /// others' did not, and enumeration failed.
    #[test]
    fn a_bridge_window_fits_below_a_root_bus_bar() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0xc080_0000, 0x0180_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let vga = PciAddress::new(0, 0, 1, 0);
        let nic = PciAddress::new(0, 1, 0, 0);
        for device in [
            endpoint(vga, &[(0, BarType::Memory32, 0x100_0000)]),
            bridge_at(PciAddress::new(0, 0, 2, 0), 1, 1),
            endpoint(nic, &[(0, BarType::Memory32, 0x10_0000)]),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(vga, PCI_BAR0) & !0xf, 0xc100_0000);
        assert_eq!(pci.read32(nic, PCI_BAR0) & !0xf, 0xc080_0000);
        assert_windows_hold_only_their_subtrees(&pci);
    }

    /// A chipset's fixed BAR, above the pool or inside it, is neither a
    /// floor for the BARs behind a bridge nor inside a bridge's window.
    #[test]
    fn a_fixed_bar_is_not_a_floor_nor_in_a_window() {
        for fixed_base in [0xfed0_0000, 0x8010_0000] {
            let (mut pci, _config_space) = test_pci();
            pci.configure_windows(0x8000_0000, 0x40_0000, 0, 0, 0x1000, 0xf000)
                .unwrap();
            let chipset = PciAddress::new(0, 0, 0x1f, 0);
            pci.add_fixed_bars(&[PciFixedBar {
                address: chipset,
                register: PCI_BAR0,
                kind: PciFixedBarType::Memory32,
                base: fixed_base,
                size: 0x10_0000,
                prefetchable: false,
            }])
            .unwrap();
            let mut fixed = endpoint(chipset, &[(0, BarType::Memory32, 0x10_0000)]);
            fixed.bars[0].allocated = true;
            fixed.bars[0].fixed = true;
            pci.write32(chipset, PCI_BAR0, fixed_base as u32);
            let disk = PciAddress::new(0, 1, 0, 0);
            for device in [
                fixed,
                bridge_at(PciAddress::new(0, 0, 2, 0), 1, 1),
                endpoint(
                    disk,
                    &[
                        (0, BarType::Memory32, 0x10_0000),
                        (1, BarType::Memory32, 0x10_0000),
                    ],
                ),
            ] {
                assert!(pci.devices.push(device).is_ok());
            }

            pci.allocate_resources().unwrap();

            assert_eq!(pci.read32(chipset, PCI_BAR0), fixed_base as u32);
            assert_windows_hold_only_their_subtrees(&pci);
        }
    }

    /// No window is placed around a fixed BAR behind a bridge.
    #[test]
    fn a_fixed_bar_behind_a_bridge_is_refused() {
        let (mut pci, _config_space) = test_pci();
        let mut fixed = endpoint(PciAddress::new(0, 1, 0, 0), &[(0, BarType::Io, 0x20)]);
        fixed.bars[0].allocated = true;
        fixed.bars[0].fixed = true;
        assert!(
            pci.devices
                .push(bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1))
                .is_ok()
        );
        assert!(pci.devices.push(fixed).is_ok());
        assert_eq!(pci.allocate_resources(), Err(PciEcamError::FixedBarInvalid));
    }

    /// Behind nested bridges, each window is its subtree, packed: the
    /// largest alignment first, the window rounded to 1 MiB.
    #[test]
    fn nested_windows_are_sized_from_their_subtrees() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x1000_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let upstream = PciAddress::new(0, 0, 1, 0);
        let downstream = PciAddress::new(0, 1, 0, 0);
        let gpu = PciAddress::new(0, 1, 1, 0);
        let disk = PciAddress::new(0, 2, 0, 0);
        let host = PciAddress::new(0, 0, 2, 0);
        for device in [
            bridge_at(upstream, 1, 2),
            bridge_at(downstream, 2, 2),
            endpoint(
                gpu,
                &[
                    (0, BarType::Memory32, 0x200_0000),
                    (1, BarType::Memory32, 0x1000),
                ],
            ),
            endpoint(disk, &[(0, BarType::Memory32, 0x4000)]),
            endpoint(host, &[(0, BarType::Memory32, 0x1000)]),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        // Upstream: the 32 MiB BAR, the 1 MiB window of the disk's port,
        // then the 4 KiB BAR, rounded up to 34 MiB.
        assert_eq!(memory_window(&pci, upstream), 0x8000_0000..=0x821f_ffff);
        assert_eq!(pci.read32(gpu, PCI_BAR0), 0x8000_0000);
        assert_eq!(memory_window(&pci, downstream), 0x8200_0000..=0x820f_ffff);
        assert_eq!(pci.read32(disk, PCI_BAR0), 0x8200_0000);
        assert_eq!(pci.read32(gpu, crate::PCI_BAR1), 0x8210_0000);
        assert_eq!(pci.read32(host, PCI_BAR0), 0x8220_0000);
        assert_windows_hold_only_their_subtrees(&pci);
    }

    /// The Ethanol-X's root bridge 0.0 (2P Milan) as its DF plan routes
    /// it: PCI memory 0xF000_0000 + 96 MiB and `ports` of I/O from 0x4000.
    /// PCIE1 (60:03.1) with a dual-port NIC, an I/O BAR on each function,
    /// comes before the BMC's link: 60:05.2, the AST1150 and the AST2500,
    /// its framebuffer, registers and ports.
    pub(super) fn ethanol_x(ports: u64) -> (PciEcam, Vec<u32>) {
        const BUSES: usize = 6;
        let mut config_space = vec![0u32; BUSES << 18];
        let root = Root {
            segment: 0,
            ecam_base: config_space.as_mut_ptr() as usize - (0x60 << 20),
            ecam_size: BUSES << 20,
            bus_start: 0x60,
            bus_end: 0x60 + BUSES as u8 - 1,
            mmio32: ResourcePool::new(0xf000_0000, 0x0600_0000).unwrap(),
            mmio64: ResourcePool::new(0x100_2020_0000, 0xdf_5fe0_0000).unwrap(),
            io_pool: ResourcePool::new(0x4000, ports).unwrap(),
            reserved_through: 0,
            next_bus: 0x61,
        };
        let mut pci = PciEcam::with_pools(HVec::from_iter([root]));
        let [pcie1, bmc, ast2500] = ethanol_x_functions();
        let nic = [0, 1].map(|function| PciAddress::new(0, 0x61, 0, function));
        let port = |function| {
            endpoint(
                nic[function],
                &[
                    (0, BarType::Memory32, 0x10_0000),
                    (2, BarType::Io, 0x20),
                    (3, BarType::Memory32, 0x4000),
                ],
            )
        };
        for device in [
            bridge_at(pcie1, 0x61, 0x61),
            port(0),
            port(1),
            bridge_at(bmc, 0x62, 0x63),
            bridge_at(PciAddress::new(0, 0x62, 0, 0), 0x63, 0x63),
            endpoint(
                ast2500,
                &[
                    (0, BarType::Memory32, 0x400_0000),
                    (1, BarType::Memory32, 0x2_0000),
                    (2, BarType::Io, 0x80),
                ],
            ),
            bridge_at(PciAddress::new(0, 0x60, 7, 1), 0x64, 0x64),
            endpoint(
                PciAddress::new(0, 0x64, 0, 2),
                &[
                    (2, BarType::Memory32, 0x8_0000),
                    (5, BarType::Memory32, 0x2000),
                ],
            ),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }
        (pci, config_space)
    }

    /// PCIE1, the BMC's link and the AST2500 of [`ethanol_x`].
    pub(super) fn ethanol_x_functions() -> [PciAddress; 3] {
        [
            PciAddress::new(0, 0x60, 3, 1),
            PciAddress::new(0, 0x60, 5, 2),
            PciAddress::new(0, 0x63, 0, 0),
        ]
    }

    /// With the 4 KiB of ports the DF's equal I/O split leaves each of
    /// eight root bridges, two bridges want an I/O window and one fits:
    /// enumeration fails, and says which; nothing is written, so nothing
    /// decodes ports it was not given.
    #[test]
    fn ports_that_do_not_fit_fail_and_nothing_is_written() {
        let (mut pci, _config_space) = ethanol_x(0x1000);
        let [pcie1, _, ast2500] = ethanol_x_functions();
        pci.write16(ast2500, PCI_COMMAND, 0);
        pci.write32(ast2500, PCI_BAR0 + 8, 0x5a5a_5a59);
        pci.write32(pcie1, PCI_IO_BASE, 0x1234);

        assert_eq!(
            pci.allocate_resources(),
            Err(PciEcamError::ResourceExhausted)
        );

        assert_eq!(pci.read16(ast2500, PCI_COMMAND), 0);
        assert_eq!(pci.read32(ast2500, PCI_BAR0 + 8), 0x5a5a_5a59);
        assert_eq!(pci.read32(pcie1, PCI_IO_BASE), 0x1234);
        for device in &pci.devices {
            assert!(!device.programmed);
            assert!(device.bars.iter().all(|bar| bar.fixed || !bar.allocated));
            assert_eq!(pci.read16(device.addr, PCI_COMMAND), 0);
        }
        assert_eq!(pci.roots[0].io_pool.free_bytes(), 0x1000);
    }

    /// With 8 KiB, each bridge's I/O window is 4 KiB: the NIC's two 32-byte
    /// I/O BARs share PCIE1's, at their own alignment, not 4 KiB each.
    #[test]
    fn io_bars_behind_a_bridge_share_its_window_at_their_own_alignment() {
        let (mut pci, _config_space) = ethanol_x(0x2000);
        let [pcie1, bmc, ast2500] = ethanol_x_functions();

        pci.allocate_resources().unwrap();

        let io_window = |bridge| {
            let value = pci.read32(bridge, PCI_IO_BASE);
            (u64::from(value & 0xf0) << 8)..=(u64::from(value & 0xf000) | 0xfff)
        };
        let nic = io_window(pcie1);
        assert_eq!(nic.end() - nic.start(), 0xfff);
        let ports: Vec<_> = [0, 1]
            .map(|function| io_bar_base(&pci, PciAddress::new(0, 0x61, 0, function), 2))
            .into();
        assert_eq!(ports[0].abs_diff(ports[1]), 0x20);
        assert!(ports.iter().all(|port| nic.contains(port)));
        let ast = io_window(bmc);
        assert!(ast.contains(&io_bar_base(&pci, ast2500, 2)));
        assert!(ast.end() < nic.start() || nic.end() < ast.start());
        for address in [pcie1, bmc, ast2500] {
            assert_ne!(pci.read16(address, PCI_COMMAND) & PCI_CMD_IO, 0);
        }
        assert_windows_hold_only_their_subtrees(&pci);
    }

    /// Small memory BARs of two functions behind a bridge share one 1 MiB
    /// window, each at its own alignment.
    #[test]
    fn small_memory_bars_behind_a_bridge_share_one_mebibyte() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x10_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let functions = [0, 1].map(|function| PciAddress::new(0, 1, 0, function));
        assert!(
            pci.devices
                .push(bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1))
                .is_ok()
        );
        for address in functions {
            let device = endpoint(
                address,
                &[
                    (0, BarType::Memory32, 0x4000),
                    (1, BarType::Memory32, 0x1000),
                ],
            );
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(
            memory_window(&pci, PciAddress::new(0, 0, 1, 0)),
            0x8000_0000..=0x800f_ffff
        );
        let mut bars: Vec<_> = pci
            .assigned_memory_bars()
            .map(|(_, bar)| bar.base)
            .collect();
        bars.sort_unstable();
        assert_eq!(bars, [0x8000_0000, 0x8000_4000, 0x8000_8000, 0x8000_9000]);
    }

    /// An I/O BAR on the root bus that does not fit fails the allocation;
    /// the function's other I/O BAR and its memory are not written, and its
    /// I/O decode stays off.
    #[test]
    fn a_root_bus_io_bar_without_ports_fails() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x10_0000, 0, 0, 0x1000, 0x40)
            .unwrap();
        let addr = PciAddress::new(0, 0, 1, 0);
        let device = endpoint(
            addr,
            &[
                (0, BarType::Io, 0x40),
                (1, BarType::Io, 0x40),
                (2, BarType::Memory32, 0x1000),
            ],
        );
        assert!(pci.devices.push(device).is_ok());

        assert_eq!(
            pci.allocate_resources(),
            Err(PciEcamError::ResourceExhausted)
        );

        for register in [PCI_BAR0, crate::PCI_BAR1, PCI_BAR0 + 8] {
            assert_eq!(pci.read32(addr, register), 0);
        }
        assert_eq!(pci.read16(addr, PCI_COMMAND), 0);
    }

    /// I/O decode follows what was placed: on for a function with I/O and
    /// memory BARs; on a bridge only with an I/O window open or an I/O BAR
    /// of its own, off otherwise even if it was on before.
    #[test]
    fn io_decode_only_where_ports_were_placed() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x100_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let with_ports = PciAddress::new(0, 0, 1, 0);
        let own_ports = PciAddress::new(0, 0, 2, 0);
        let without = PciAddress::new(0, 0, 3, 0);
        let mixed = PciAddress::new(0, 1, 0, 0);
        let memory_only = PciAddress::new(0, 2, 0, 0);
        let mut own = bridge_at(own_ports, 2, 2);
        own.bars[1] = endpoint(own_ports, &[(1, BarType::Io, 0x10)]).bars[1];
        let mut closed = bridge_at(without, 3, 3);
        closed.original_command = PCI_CMD_IO;
        for device in [
            bridge_at(with_ports, 1, 1),
            own,
            closed,
            endpoint(
                mixed,
                &[(0, BarType::Io, 0x20), (1, BarType::Memory32, 0x1000)],
            ),
            endpoint(memory_only, &[(0, BarType::Memory32, 0x1000)]),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }
        pci.write16(without, PCI_COMMAND, PCI_CMD_IO);

        pci.allocate_resources().unwrap();

        let command = |address| pci.read16(address, PCI_COMMAND) & (PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(command(mixed), PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(command(memory_only), PCI_CMD_MEMORY);
        assert_eq!(command(with_ports), PCI_CMD_IO | PCI_CMD_MEMORY);
        // Its own I/O BAR; behind it only memory, its I/O window closed.
        assert_eq!(pci.read32(own_ports, PCI_IO_BASE) & 0xffff, 0x00ff);
        assert_eq!(command(own_ports), PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(command(without), PCI_CMD_MEMORY);
        assert!(pci.devices.iter().all(|device| device.programmed));
    }

    /// The prefetchable window `bridge` decodes, inclusive, its upper
    /// halves read as they are.
    pub(super) fn prefetchable_window(
        pci: &PciEcam,
        bridge: PciAddress,
    ) -> core::ops::RangeInclusive<u64> {
        let value = pci.read32(bridge, PCI_PREF_MEMORY_BASE);
        let base = (u64::from(pci.read32(bridge, PCI_PREF_BASE_UPPER32)) << 32)
            | u64::from((value & 0xfff0) << 16);
        let end = (u64::from(pci.read32(bridge, PCI_PREF_LIMIT_UPPER32)) << 32)
            | u64::from(value & 0xfff0_0000)
            | 0xf_ffff;
        base..=end
    }

    /// Behind one bridge, a 1 MiB BAR and a 4 MiB 32-bit prefetchable one
    /// in [0x8030_0000, 0x8080_0000): the memory window takes the first,
    /// the prefetchable window, below 4 GiB, the other, as their spans did
    /// when BARs were placed one by one. One window for both, aligned to
    /// 4 MiB, does not fit.
    #[test]
    fn a_32bit_prefetchable_bar_takes_the_prefetchable_window_below_4_gib() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8030_0000, 0x50_0000, 0x1_0000_0000, 0x1000_0000, 0, 0)
            .unwrap();
        let bridge = PciAddress::new(0, 0, 1, 0);
        let device = PciAddress::new(0, 1, 0, 0);
        let mut endpoint = endpoint(
            device,
            &[
                (0, BarType::Memory32, 0x10_0000),
                (1, BarType::Memory32, 0x40_0000),
            ],
        );
        endpoint.bars[1].prefetchable = true;
        for device in [bridge_at(bridge, 1, 1), endpoint] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(device, PCI_BAR0), 0x8030_0000);
        assert_eq!(pci.read32(device, crate::PCI_BAR1), 0x8040_0008);
        assert_eq!(memory_window(&pci, bridge), 0x8030_0000..=0x803f_ffff);
        assert_eq!(prefetchable_window(&pci, bridge), 0x8040_0000..=0x807f_ffff);
    }

    /// Two 64-bit prefetchable BARs, 8 and 4 MiB, behind one bridge, with 8
    /// MiB below 4 GiB and 4 MiB above: the 8 MiB one below through the
    /// memory window, the 4 MiB one above through the prefetchable window.
    #[test]
    fn prefetchable_memory_splits_across_4_gib() {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x80_0000, 0x1_0000_0000, 0x40_0000, 0, 0)
            .unwrap();
        let bridge = PciAddress::new(0, 0, 1, 0);
        let device = PciAddress::new(0, 1, 0, 0);
        let mut endpoint = endpoint(
            device,
            &[
                (0, BarType::Memory64, 0x80_0000),
                (2, BarType::Memory64, 0x40_0000),
            ],
        );
        endpoint.bars[0].prefetchable = true;
        endpoint.bars[2].prefetchable = true;
        for device in [bridge_at(bridge, 1, 1), endpoint] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(device, PCI_BAR0), 0x8000_000c);
        assert_eq!(pci.read32(device, crate::PCI_BAR1), 0);
        assert_eq!(pci.read32(device, PCI_BAR0 + 8), 0x0000_000c);
        assert_eq!(pci.read32(device, PCI_BAR0 + 12), 1);
        assert_eq!(memory_window(&pci, bridge), 0x8000_0000..=0x807f_ffff);
        assert_eq!(
            prefetchable_window(&pci, bridge),
            0x1_0000_0000..=0x1_003f_ffff
        );
    }

    /// A bridge whose prefetchable window is 32-bit keeps the 64-bit
    /// prefetchable BARs behind it below 4 GiB and its upper registers
    /// unwritten; its parent, 64-bit, forwards that window through its
    /// memory window while a sibling's 64-bit BAR, which does not fit
    /// below, goes above through its prefetchable window. A bridge without
    /// one forwards prefetchable memory through its memory window.
    #[test]
    fn prefetchable_windows_as_wide_as_the_bridge_has() {
        let (mut pci, _config_space) = test_pci_with_buses(5);
        pci.configure_windows(0x8000_0000, 0x40_0000, 0x1_0000_0000, 0x100_0000, 0, 0)
            .unwrap();
        let parent = PciAddress::new(0, 0, 1, 0);
        let narrow = PciAddress::new(0, 1, 0, 0);
        let wide = PciAddress::new(0, 1, 1, 0);
        let none = PciAddress::new(0, 0, 2, 0);
        let behind_narrow = PciAddress::new(0, 2, 0, 0);
        let behind_wide = PciAddress::new(0, 3, 0, 0);
        let behind_none = PciAddress::new(0, 4, 0, 0);
        let prefetchable = |address, size| {
            let mut device = endpoint(address, &[(0, BarType::Memory64, size)]);
            device.bars[0].prefetchable = true;
            device
        };
        let mut narrow_bridge = bridge_at(narrow, 2, 2);
        narrow_bridge.prefetch = PrefetchWindow::Bits32;
        let mut none_bridge = bridge_at(none, 4, 4);
        none_bridge.prefetch = PrefetchWindow::None;
        for device in [
            bridge_at(parent, 1, 3),
            narrow_bridge,
            bridge_at(wide, 3, 3),
            prefetchable(behind_narrow, 0x10_0000),
            prefetchable(behind_wide, 0x80_0000),
            none_bridge,
            prefetchable(behind_none, 0x10_0000),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }
        for bridge in [narrow, none] {
            pci.write32(bridge, PCI_PREF_BASE_UPPER32, 0xa5a5_a5a5);
            pci.write32(bridge, PCI_PREF_LIMIT_UPPER32, 0xa5a5_a5a5);
        }
        pci.write32(none, PCI_PREF_MEMORY_BASE, 0x5a5a_5a5a);

        pci.allocate_resources().unwrap();

        let base = |address| {
            u64::from(pci.read32(address, PCI_BAR0) & !0xf)
                | u64::from(pci.read32(address, crate::PCI_BAR1)) << 32
        };
        assert!(base(behind_narrow) < 1 << 32);
        assert!(base(behind_wide) >= 1 << 32);
        assert!(base(behind_none) < 1 << 32);
        for bridge in [narrow, none] {
            assert_eq!(pci.read32(bridge, PCI_PREF_BASE_UPPER32), 0xa5a5_a5a5);
            assert_eq!(pci.read32(bridge, PCI_PREF_LIMIT_UPPER32), 0xa5a5_a5a5);
        }
        assert_eq!(pci.read32(none, PCI_PREF_MEMORY_BASE), 0x5a5a_5a5a);
        assert!(memory_window(&pci, none).contains(&base(behind_none)));
        let narrow_window = prefetchable_window(&pci, narrow);
        assert_eq!(*narrow_window.end() >> 32, 0xa5a5_a5a5);
        let narrow_low =
            (narrow_window.start() & 0xffff_ffff)..=(narrow_window.end() & 0xffff_ffff);
        assert!(narrow_low.contains(&base(behind_narrow)));
        assert!(memory_window(&pci, parent).contains(narrow_low.start()));
        assert!(memory_window(&pci, parent).contains(narrow_low.end()));
        assert!(prefetchable_window(&pci, parent).contains(&base(behind_wide)));
        assert!(prefetchable_window(&pci, wide).contains(&base(behind_wide)));
    }

    /// A bridge's prefetchable window as enumeration reads it: 64-bit when
    /// its base says so and its upper base holds bits, 32-bit when it says
    /// 32-bit; a base that reads 0 is tried with ones and put back.
    #[test]
    fn the_prefetchable_window_is_read_as_linux_reads_it() {
        let (pci, _config_space) = test_pci();
        let bridge = PciAddress::new(0, 0, 1, 0);
        pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0x0001_0001);
        pci.write32(bridge, PCI_PREF_BASE_UPPER32, 0x1234);
        assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits64);
        assert_eq!(pci.read32(bridge, PCI_PREF_BASE_UPPER32), 0x1234);
        pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0x0000_fff0);
        assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits32);
        pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0);
        // RAM holds the ones: a 32-bit window, its register put back.
        assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits32);
        assert_eq!(pci.read32(bridge, PCI_PREF_MEMORY_BASE), 0);
    }

    pub(super) fn endpoint(addr: PciAddress, definitions: &[(usize, BarType, u64)]) -> PciDev {
        let none = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg: 0,
            allocated: false,
            fixed: false,
            base: 0,
            side: Side::Low,
        };
        let mut bars = [none; 6];
        for &(index, bar_type, size) in definitions {
            bars[index] = BarInfo {
                bar_type,
                size,
                prefetchable: false,
                reg: PCI_BAR0 + index as u16 * 4,
                allocated: false,
                fixed: false,
                base: 0,
                side: Side::Low,
            };
        }
        PciDev {
            addr,
            root: 0,
            header_type: 0,
            original_command: 0,
            bars,
            secondary_bus: 0,
            subordinate_bus: 0,
            windows: Default::default(),
            // What a bridge built from it has: plain RAM cannot model the
            // read-only capability bits enumeration probes.
            prefetch: PrefetchWindow::Bits64,
            parent: None,
            programmed: false,
        }
    }

    pub(super) fn io_bar_base(pci: &PciEcam, addr: PciAddress, index: usize) -> u64 {
        u64::from(pci.read32(addr, PCI_BAR0 + index as u16 * 4) & 0x0000_fffc)
    }

    #[test]
    fn a_root_becomes_its_pools() {
        let root = PciRoot::new(
            PciRootInfo {
                segment: 3,
                ecam_base: 0xe000_0000,
                bus_start: 0x40,
                bus_end: 0x7f,
            },
            [
                Window.mmio(0x8000_0000, 0x1000_0000),
                Window.io(0x1000, 0xf000),
            ],
        )
        .unwrap();
        let pci = PciEcam::new([root.clone()]).unwrap();

        assert_eq!(pci.roots[0].segment, 3);
        assert_eq!(pci.ecam_base(), 0xe000_0000);
        assert_eq!(pci.ecam_size(), 64 * 1024 * 1024);
        assert_eq!(pci.bus_start(), 0x40);
        assert_eq!(pci.bus_end(), 0x7f);
        assert_eq!(pci.roots[0].mmio32.free_bytes(), 0x1000_0000);
        assert_eq!(pci.roots[0].mmio64.free_bytes(), 0);
        assert_eq!(pci.roots[0].io_pool.free_bytes(), 0xf000);
    }

    #[test]
    fn a_root_is_refused_rather_than_cut_short() {
        let info = PciRootInfo {
            segment: 0,
            ecam_base: 0xe000_0000,
            bus_start: 0,
            bus_end: 0xff,
        };
        // Windows that do not touch stay apart: one too many.
        let many = (0..=crate::MAX_PCI_ROOT_WINDOWS as u64)
            .map(|index| Window.mmio(0x8000_0000 + (index << 21), 0x10_0000));
        assert_eq!(
            PciRoot::new(info, many).unwrap_err(),
            PciEcamError::ResourceRangeLimit
        );
        for wrong in [
            Window.io(0xff00, 0x200),
            Window.mmio(u64::MAX - 0xfff, 0x2000),
        ] {
            assert_eq!(
                PciRoot::new(info, [wrong]).unwrap_err(),
                PciEcamError::ConfigError
            );
        }
        let backwards = PciRootInfo {
            bus_start: 1,
            bus_end: 0,
            ..info
        };
        assert!(PciRoot::new(backwards, []).is_err());
    }

    /// A root bridge of fixed buses with one MMIO window.
    fn domain(info: PciRootInfo, mmio: u64) -> PciRoot {
        PciRoot::new(info, [Window.mmio(mmio, 0x1000_0000)]).unwrap()
    }

    #[test]
    fn every_root_allocates_its_buses_and_windows() {
        // Buses 0..=0x41 of test RAM, in which every BAR sizes as 16 bytes
        // of 32-bit memory. Each root has a bridge with an endpoint behind it.
        let mut ecam = vec![0u32; 0x42 << 18];
        let base = ecam.as_mut_ptr() as usize;
        let config = |bus: u8, device: u8, offset: usize| {
            (usize::from(bus) << 18) + (usize::from(device) << 13) + offset / 4
        };
        for bus in [0, 1, 0x40, 0x41] {
            (0..32).for_each(|device| ecam[config(bus, device, 0)] = u32::MAX);
        }
        for (bus, header, class) in [
            (0, 1, 0x0604),
            (1, 0, 0x0200),
            (0x40, 1, 0x0604),
            (0x41, 0, 0x0200),
        ] {
            ecam[config(bus, 0, 0)] = 0x1234_1022;
            ecam[config(bus, 0, 8)] = class << 16;
            ecam[config(bus, 0, 0xc)] = header << 16;
        }
        let root = |bus_start, bus_end, window| {
            let info = PciRootInfo {
                segment: 0,
                ecam_base: base as u64,
                bus_start,
                bus_end,
            };
            domain(info, window)
        };
        let roots = [root(0, 0x3f, 0x8000_0000), root(0x40, 0x7f, 0xa000_0000)];
        assert!(matches!(
            PciEcam::new([root(0, 0x40, 0), root(0x40, 0x7f, 0)]),
            Err(PciEcamError::ConfigError)
        ));
        let mut pci = PciEcam::new(roots.clone()).unwrap();

        pci.enumerate_and_allocate().unwrap();

        let buses = |bus| pci.read32(PciAddress::new(0, bus, 0, 0), PCI_PRIMARY_BUS) & 0xff_ffff;
        assert_eq!((buses(0), buses(0x40)), (0x01_0100, 0x41_4140));
        for (endpoint, window) in [
            (1, 0x8000_0000..0x9000_0000),
            (0x41, 0xa000_0000..0xb000_0000),
        ] {
            let endpoint = PciAddress::new(0, endpoint, 0, 0);
            let bar = u64::from(pci.read32(endpoint, PCI_BAR0) & !0xf);
            assert!(window.contains(&bar), "{endpoint:?}: {bar:#x}");
        }
    }

    /// A host bridge in the shape of an Intel one: everything that is not
    /// DRAM forwarded, the ECAM window and a chipset BAR its own.
    fn described() -> PciRoot {
        PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: 0xe000_0000,
                bus_start: 0,
                bus_end: 0xff,
            },
            [
                Forwarded.io(0, 0x1_0000),
                Window.io(0x1000, 0xf000),
                Forwarded.mmio(0xc_0000, 0x4_0000),
                Window.mmio(0x8000_0000, 0x7ec0_0000),
                Kept.mmio(0xfed4_0000, 0x5000),
                Window.mmio(0x1_0000_0000, 0x1_0000_0000),
                Own.mmio(0xe000_0000, 0x1000_0000),
                Own.mmio(0xfed1_0000, 0x4000),
                // Up against the range that is kept.
                Own.mmio(0xfed2_0000, 0x2_0000),
            ],
        )
        .unwrap()
    }

    fn spans(ranges: &[PciWindow]) -> Vec<(PciWindowKind, u64, u64)> {
        ranges.iter().map(|r| (r.kind, r.base, r.end())).collect()
    }

    #[test]
    fn the_three_readers_of_a_root() {
        use PciWindowKind::{Io, Mmio};
        let root = described();
        // `_CRS`: all that is forwarded, without what the chipset decodes.
        assert_eq!(
            spans(root.forwarded()),
            [
                (Io, 0, 0x1_0000),
                (Mmio, 0xc_0000, 0x10_0000),
                (Mmio, 0x8000_0000, 0xe000_0000),
                (Mmio, 0xf000_0000, 0xfec0_0000),
                (Mmio, 0xfed4_0000, 0xfed4_5000),
                (Mmio, 0x1_0000_0000, 0x2_0000_0000),
            ]
        );
        // The allocator: the windows without anything kept or owned.
        assert_eq!(
            spans(root.allocatable()),
            [
                (Io, 0x1000, 0x1_0000),
                (Mmio, 0x8000_0000, 0xe000_0000),
                (Mmio, 0xf000_0000, 0xfec0_0000),
                (Mmio, 0x1_0000_0000, 0x2_0000_0000),
            ]
        );
        // The motherboard device: what is kept and what is owned, each as
        // described. The kept range is in a window of the bridge and its
        // neighbour is not: as one range an OS could reserve neither.
        assert_eq!(
            spans(root.motherboard()),
            [
                (Mmio, 0xfed4_0000, 0xfed4_5000),
                (Mmio, 0xe000_0000, 0xf000_0000),
                (Mmio, 0xfed1_0000, 0xfed1_4000),
                (Mmio, 0xfed2_0000, 0xfed4_0000),
            ]
        );
        let mut pci = PciEcam::new([root.clone()]).unwrap();
        let pools = &mut pci.roots[0];
        assert_eq!(pools.io_pool.allocate_aligned(0x100, 0x100), Some(0x1000));
        assert_eq!(pools.mmio32.free_bytes(), 0x6000_0000 + 0x0ec0_0000);
        assert_eq!(pools.mmio64.free_bytes(), 0x1_0000_0000);
    }

    #[test]
    fn ranges_that_touch_are_one_in_the_order_of_description() {
        let info = described().info();
        let root = PciRoot::new(
            info,
            [
                // A routed window, the PCI pools in it and the non-PCI one.
                Forwarded.mmio(0xf000_0000, 0x0ec0_0000),
                Window.mmio(0xf000_0000, 0x0d00_0000),
                Own.mmio(0xfd00_0000, 0x0100_0000),
                Window.io(0x1000, 0x4000),
                Forwarded.io(0, 0x1000),
                // Joins the two above it once it is there.
                Forwarded.mmio(0xb000_0000, 0x1000_0000),
                Forwarded.mmio(0xd000_0000, 0x1000_0000),
                Forwarded.mmio(0xc000_0000, 0x1000_0000),
            ],
        )
        .unwrap();
        use PciWindowKind::{Io, Mmio};
        assert_eq!(
            spans(root.forwarded()),
            [
                (Mmio, 0xf000_0000, 0xfd00_0000),
                (Mmio, 0xfe00_0000, 0xfec0_0000),
                (Io, 0, 0x5000),
                (Mmio, 0xb000_0000, 0xe000_0000),
            ]
        );
        assert_eq!(
            spans(root.allocatable()),
            [(Mmio, 0xf000_0000, 0xfd00_0000), (Io, 0x1000, 0x5000)]
        );
    }

    /// The agreement of the tables with the allocator: whatever is
    /// described, every address the allocator may give out is in a range
    /// `_CRS` reports, and in no motherboard resource.
    #[test]
    fn what_the_allocator_may_use_is_forwarded_and_nobodys() {
        // A small deterministic generator: descriptions of every shape.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        let info = described().info();
        let mut described = 0;
        for _ in 0..20_000 {
            let resources: Vec<_> = (0..next(9))
                .map(|_| {
                    let role = [Window, Window, Forwarded, Kept, Own][next(5) as usize];
                    let (base, size) = (next(60) << 12, (1 + next(24)) << 12);
                    match next(3) {
                        0 => role.io(base % 0xf000, size.min(0x1000)),
                        _ => role.mmio(0x8000_0000 + base, size),
                    }
                })
                .collect();
            let Ok(root) = PciRoot::new(info, resources.iter().copied()) else {
                continue;
            };
            described += 1;
            let inside = |part: &PciWindow, whole: &PciWindow| {
                part.kind == whole.kind && whole.base <= part.base && part.end() <= whole.end()
            };
            let overlaps = |a: &PciWindow, b: &PciWindow| {
                a.kind == b.kind && a.base < b.end() && b.base < a.end()
            };
            for usable in root.allocatable() {
                assert!(
                    root.forwarded().iter().any(|whole| inside(usable, whole)),
                    "{usable:x?} not forwarded: {resources:x?}"
                );
                assert!(
                    !root.motherboard().iter().any(|kept| overlaps(usable, kept)),
                    "{usable:x?} is a motherboard resource: {resources:x?}"
                );
                // And it is what was described: in a window, and off what
                // is kept and owned.
                assert!(
                    resources
                        .iter()
                        .any(|r| r.role == Window && overlaps(usable, &r.range))
                );
                for resource in &resources {
                    let taken = matches!(resource.role, Kept | Own);
                    assert!(!taken || !overlaps(usable, &resource.range));
                }
            }
            // Nothing the chipset decodes itself is forwarded.
            for resource in resources.iter().filter(|r| r.role == Own) {
                assert!(
                    !root
                        .forwarded()
                        .iter()
                        .any(|whole| overlaps(whole, &resource.range)),
                    "{resource:x?} forwarded: {resources:x?}"
                );
            }
            // The views are disjoint lists.
            for view in [root.forwarded(), root.allocatable()] {
                for (index, range) in view.iter().enumerate() {
                    assert!(!view[index + 1..].iter().any(|other| overlaps(range, other)));
                }
            }
        }
        assert!(described > 10_000);
    }

    /// Two SP3 sockets: eight root bridges with 160 functions between them.
    #[test]
    fn eight_roots_hold_the_functions_of_two_sockets() {
        const ROOTS: u8 = 8;
        const FUNCTIONS: u8 = 20;
        // One bus of test RAM per root, 0x20 buses apart; every BAR sizes as
        // 16 bytes of 32-bit memory.
        let mut ecam = vec![0u32; (usize::from(ROOTS - 1) * 0x20 + 1) << 18];
        let base = ecam.as_mut_ptr() as usize;
        let config = |bus: u8, device: u8, offset: usize| {
            (usize::from(bus) << 18) + (usize::from(device) << 13) + offset / 4
        };
        for root in 0..ROOTS {
            let bus = root * 0x20;
            (0..32).for_each(|device| ecam[config(bus, device, 0)] = u32::MAX);
            for device in 0..FUNCTIONS {
                ecam[config(bus, device, 0)] = 0x1234_1022;
                ecam[config(bus, device, 8)] = 0x0200 << 16;
            }
        }
        let roots: Vec<PciRoot> = (0..ROOTS)
            .map(|root| {
                let info = PciRootInfo {
                    segment: 0,
                    ecam_base: base as u64,
                    bus_start: root * 0x20,
                    bus_end: root * 0x20 + 0x1f,
                };
                domain(info, 0x8000_0000 + u64::from(root) * 0x1000_0000)
            })
            .collect();
        let mut pci = PciEcam::new(roots.clone()).unwrap();

        pci.enumerate_and_allocate().unwrap();

        assert_eq!(
            pci.device_count(),
            usize::from(ROOTS) * usize::from(FUNCTIONS)
        );
        for root in 0..ROOTS {
            let window = 0x8000_0000 + u64::from(root) * 0x1000_0000;
            for device in [0, FUNCTIONS - 1] {
                let function = PciAddress::new(0, root * 0x20, device, 0);
                let bar = u64::from(pci.read32(function, PCI_BAR0) & !0xf);
                assert!(
                    (window..window + 0x1000_0000).contains(&bar),
                    "{function:?}: {bar:#x}"
                );
            }
        }
    }

    #[test]
    fn a_root_does_not_take_bus_numbers_beyond_its_range() {
        let mut ecam = vec![0u32; 2 << 18];
        (0..32).for_each(|device| ecam[device << 13] = u32::MAX);
        ecam[0] = 0x1234_1022;
        ecam[2] = 0x0604 << 16;
        ecam[3] = 1 << 16;
        let info = PciRootInfo {
            segment: 0,
            ecam_base: ecam.as_mut_ptr() as u64,
            bus_start: 0,
            bus_end: 0,
        };
        let mut pci = PciEcam::new([domain(info, 0x8000_0000)]).unwrap();
        assert_eq!(
            pci.enumerate_and_allocate(),
            Err(PciEcamError::ResourceExhausted)
        );
    }

    #[test]
    fn bar_size_uses_moving_bits_not_the_all_ones_value() {
        // Bit 4 reads as one for both probe values, while bit 5 is the first
        // address bit software can move. A one-mask would incorrectly report
        // 16 bytes; the PCI moving-bit algorithm reports 32.
        let ones = 0xffff_fff1u32;
        let zeroes = 0x0000_0011u32;
        assert_eq!(
            size_from_moving_bits(u64::from(ones ^ zeroes), 0x0000_fffc),
            Ok(0x20)
        );
    }

    #[test]
    fn wc_selection_excludes_gpu_registers_and_non_display_prefetchable_bars() {
        let (mut pci, _config_space) = test_pci();
        let vga = PciAddress::new(0, 0, 2, 0);
        let storage = PciAddress::new(0, 0, 3, 0);
        for (address, class) in [(vga, 0x0300_0000), (storage, 0x0108_0000)] {
            let mut device = endpoint(
                address,
                &[
                    (0, BarType::Memory32, 0x10_0000),
                    (2, BarType::Memory32, 0x100_0000),
                ],
            );
            device.bars[2].prefetchable = true;
            assert!(pci.devices.push(device).is_ok());
            pci.write32(address, 8, class);
        }
        pci.allocate_resources().unwrap();
        let windows: HVec<_, 6> = pci
            .prefetchable_vga_windows()
            .map(|window| (window.base, window.size))
            .collect();
        assert_eq!(
            windows.as_slice(),
            &[(u64::from(pci.read32(vga, PCI_BAR0 + 8) & !0xf), 0x100_0000)]
        );
    }

    #[test]
    fn assigned_bar_inspection_includes_io_without_changing_memory_consumers() {
        let (mut pci, _space) = test_pci();
        pci.configure_windows(0xb0000000, 0x1000000, 0, 0, 0x4000, 0x4000)
            .unwrap();
        let address = PciAddress::new(0, 0, 1, 0);
        assert!(
            pci.devices
                .push(endpoint(
                    address,
                    &[(0, BarType::Memory32, 0x100000), (1, BarType::Io, 0x80)]
                ))
                .is_ok()
        );
        pci.allocate_resources().unwrap();
        let all: HVec<_, 6> = pci.assigned_bars().collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].1.kind, PciWindowKind::Mmio);
        assert_eq!(all[1].1.kind, PciWindowKind::Io);
        assert_eq!((all[1].1.base, all[1].1.size), (0x4000, 0x80));
        assert_eq!(pci.assigned_memory_bars().count(), 1);
        assert_eq!(pci.assigned_memory_windows().count(), 1);
    }

    #[test]
    fn graphics_mmio_bar_reuses_gap_before_aligned_framebuffer() {
        let (mut pci, _config_space) = test_pci();
        // X61's aperture runs from TOLUD (3 GiB) to ECAM (3.5 GiB).
        pci.configure_windows(0xc000_0000, 0x2000_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let igd = PciAddress::new(0, 0, 2, 0);
        let hda = PciAddress::new(0, 0, 0x1b, 0);
        assert!(
            pci.devices
                .push(endpoint(
                    igd,
                    &[
                        (0, BarType::Memory64, 0x10_0000),
                        (2, BarType::Memory64, 0x1000_0000),
                    ],
                ))
                .is_ok()
        );
        assert!(
            pci.devices
                .push(endpoint(hda, &[(0, BarType::Memory32, 0x4000)]))
                .is_ok()
        );

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(hda, PCI_BAR0) & !0xf, 0xc000_0000);
        assert_eq!(pci.read32(igd, PCI_BAR0 + 8) & !0xf, 0xd000_0000);
        assert_eq!(pci.read32(igd, PCI_BAR0) & !0xf, 0xc010_0000);
        let windows: HVec<_, 6> = pci
            .assigned_memory_windows()
            .map(|window| (window.base, window.size))
            .collect();
        assert_eq!(
            windows.as_slice(),
            &[
                (0xc010_0000, 0x10_0000),
                (0xd000_0000, 0x1000_0000),
                (0xc000_0000, 0x4000),
            ]
        );
    }

    #[test]
    fn firmware_io_reservations_exclude_dynamic_bars() {
        let (mut pci, _memory) = test_pci();
        pci.configure_windows(0x8000_0000, 0x1000_0000, 0, 0, 0x15c0, 0x200)
            .unwrap();
        // EC/PMH7, battery interface and both dock-controller windows.
        let reserved = [
            (0x15e0u64, 16u64),
            (0x1600, 1),
            (0x1602, 1),
            (0x1604, 1),
            (0x1606, 1),
            (0x1610, 16),
            (0x1620, 16),
            (0x164c, 4),
            (0x1680, 16),
        ];
        for (base, size) in reserved {
            pci.reserve_io_range(base, size).unwrap();
        }
        assert!(pci.reserve_io_range(0xffff, 2).is_err());
        for index in 0..28 {
            let size = if index < 24 { 16 } else { 4 };
            assert!(
                pci.devices
                    .push(endpoint(
                        PciAddress::new(0, 0, index + 1, 0),
                        &[(0, BarType::Io, size)]
                    ))
                    .is_ok()
            );
        }
        pci.allocate_resources().unwrap();
        for device in &pci.devices {
            let base = io_bar_base(&pci, device.addr, 0);
            let size = device.bars[0].size;
            assert!(base >= 0x15c0 && base + size <= 0x17c0);
            for (start, length) in reserved {
                assert!(
                    base + size <= start || start + length <= base,
                    "BAR overlaps firmware I/O"
                );
            }
        }
    }

    #[test]
    fn fixed_bar_is_preserved_without_consuming_dynamic_space() {
        let (mut pci, _config_space) = test_pci();
        let fixed = PciFixedBar {
            address: PciAddress::new(0, 0, 0x1f, 3),
            register: PCI_BAR0 + 4 * 4,
            kind: PciFixedBarType::Io,
            base: 0x1040,
            size: 0x20,
            prefetchable: false,
        };
        pci.add_fixed_bars(&[fixed]).unwrap();

        let io_pool = &mut pci.roots[0].io_pool;
        assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1000));
        assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1020));
        assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1060));
    }

    #[test]
    fn fixed_bar_probe_preserves_bar_and_restores_original_decode_policy() {
        let (mut pci, _config_space) = test_pci();
        let smbus = PciAddress::new(0, 0, 0x1f, 3);
        pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
        pci.device(smbus).unwrap().write8(PCI_HEADER_TYPE, 0);
        pci.write16(smbus, PCI_COMMAND, PCI_CMD_IO);
        pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x401);
        pci.add_fixed_bars(&[PciFixedBar {
            address: smbus,
            register: PCI_BAR0 + 4 * 4,
            kind: PciFixedBarType::Io,
            base: 0x400,
            size: 0x20,
            prefetchable: false,
        }])
        .unwrap();

        let dev = pci.probe_device(smbus, true).unwrap().unwrap();
        assert_ne!(pci.read16(smbus, PCI_COMMAND) & PCI_CMD_IO, 0);
        assert!(dev.bars[4].fixed);
        assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
        assert!(pci.devices.push(dev).is_ok());

        pci.allocate_resources().unwrap();

        assert_ne!(pci.read16(smbus, PCI_COMMAND) & PCI_CMD_IO, 0);
        assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
    }

    #[test]
    fn barless_device_retains_non_bar_decode_policy() {
        let (mut pci, _config_space) = test_pci();
        let lpc = PciAddress::new(0, 0, 0x1f, 0);
        let mut dev = endpoint(lpc, &[]);
        dev.original_command = PCI_CMD_IO | PCI_CMD_MEMORY;
        assert!(pci.devices.push(dev).is_ok());

        pci.allocate_resources().unwrap();

        let command = pci.read16(lpc, PCI_COMMAND);
        assert_eq!(
            command & (PCI_CMD_IO | PCI_CMD_MEMORY),
            PCI_CMD_IO | PCI_CMD_MEMORY
        );
    }

    #[test]
    fn host_bridge_chipset_registers_are_not_probed_as_bars() {
        let (pci, _config_space) = test_pci();
        let host = PciAddress::new(0, 0, 0, 0);
        pci.write32(host, PCI_VENDOR_ID, 0xa000_8086);
        pci.write32(host, 0x08, 0x0600_0000);
        pci.device(host).unwrap().write8(PCI_HEADER_TYPE, 0);
        pci.write16(host, PCI_COMMAND, PCI_CMD_MEMORY);
        pci.write32(host, PCI_BAR0, 0xdead_beef);

        let dev = pci.probe_device(host, true).unwrap().unwrap();

        assert!(dev.bars.iter().all(|bar| bar.bar_type == BarType::None));
        assert_eq!(pci.read32(host, PCI_BAR0), 0xdead_beef);
        assert_eq!(pci.read16(host, PCI_COMMAND), PCI_CMD_MEMORY);
    }

    #[test]
    fn fixed_bar_probe_rejects_live_base_mismatch() {
        let (mut pci, _config_space) = test_pci();
        let smbus = PciAddress::new(0, 0, 0x1f, 3);
        pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
        pci.device(smbus).unwrap().write8(PCI_HEADER_TYPE, 0);
        pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x421);
        pci.add_fixed_bars(&[PciFixedBar {
            address: smbus,
            register: PCI_BAR0 + 4 * 4,
            kind: PciFixedBarType::Io,
            base: 0x400,
            size: 0x20,
            prefetchable: false,
        }])
        .unwrap();

        assert!(matches!(
            pci.probe_device(smbus, true),
            Err(PciEcamError::FixedBarInvalid)
        ));
    }

    #[test]
    fn d41s_fixed_smbus_does_not_displace_sata_bar() {
        let (mut pci, _config_space) = test_pci();
        let igd = PciAddress::new(0, 0, 0x02, 0);
        let sata = PciAddress::new(0, 0, 0x1f, 2);
        let smbus = PciAddress::new(0, 0, 0x1f, 3);

        pci.add_fixed_bars(&[PciFixedBar {
            address: smbus,
            register: PCI_BAR0 + 4 * 4,
            kind: PciFixedBarType::Io,
            base: 0x400,
            size: 0x20,
            prefetchable: false,
        }])
        .unwrap();

        assert!(
            pci.devices
                .push(endpoint(igd, &[(1, BarType::Io, 0x8)]))
                .is_ok()
        );
        for function in 0..4 {
            assert!(
                pci.devices
                    .push(endpoint(
                        PciAddress::new(0, 0, 0x1d, function),
                        &[(4, BarType::Io, 0x20)],
                    ))
                    .is_ok()
            );
        }
        assert!(
            pci.devices
                .push(endpoint(
                    sata,
                    &[
                        (0, BarType::Io, 0x8),
                        (1, BarType::Io, 0x4),
                        (2, BarType::Io, 0x8),
                        (3, BarType::Io, 0x4),
                        (4, BarType::Io, 0x20),
                    ],
                ))
                .is_ok()
        );
        let mut smbus_dev = endpoint(smbus, &[]);
        smbus_dev.bars[4] = BarInfo {
            bar_type: BarType::Io,
            size: 0x20,
            prefetchable: false,
            reg: PCI_BAR0 + 4 * 4,
            allocated: true,
            fixed: true,
            base: 0,
            side: Side::Low,
        };
        assert!(pci.devices.push(smbus_dev).is_ok());
        pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x401);

        pci.allocate_resources().unwrap();

        assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
        assert_eq!(io_bar_base(&pci, sata, 4), 0x1080);

        let mut intervals = Vec::new();
        for dev in &pci.devices {
            for (index, bar) in dev.bars.iter().enumerate() {
                if bar.bar_type != BarType::Io || !bar.allocated {
                    continue;
                }
                let base = io_bar_base(&pci, dev.addr, index);
                intervals.push((base, base + bar.size));
            }
        }
        for left in 0..intervals.len() {
            for right in left + 1..intervals.len() {
                assert!(
                    intervals[left].1 <= intervals[right].0
                        || intervals[right].1 <= intervals[left].0,
                    "overlapping intervals: {:?} and {:?}",
                    intervals[left],
                    intervals[right]
                );
            }
        }
    }

    #[test]
    fn conflicting_fixed_bars_are_rejected() {
        let (mut pci, _config_space) = test_pci();
        let first = PciFixedBar {
            address: PciAddress::new(0, 0, 1, 0),
            register: PCI_BAR0,
            kind: PciFixedBarType::Io,
            base: 0x1000,
            size: 0x20,
            prefetchable: false,
        };
        let second = PciFixedBar {
            address: PciAddress::new(0, 0, 2, 0),
            register: PCI_BAR0,
            kind: PciFixedBarType::Io,
            base: 0x1010,
            size: 0x10,
            prefetchable: false,
        };
        pci.add_fixed_bars(&[first]).unwrap();
        assert_eq!(
            pci.add_fixed_bars(&[second]),
            Err(PciEcamError::FixedBarConflict)
        );
    }
}
