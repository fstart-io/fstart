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
    /// The window search ran out of its budget, of candidate windows or of
    /// work, before it found a layout or proved there is none.
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
/// Unmanaged type-0 functions: never probe BARs or change command.
/// RO slot-presence/header reads may still be needed to discover siblings.
pub type PciExcludedFunctions = HVec<PciAddress, 32>;

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

use pool::{
    MAX_RESOURCE_RANGES, ResourcePool, bar_unimplemented, fixed_bars_overlap, size_from_moving_bits,
};

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
    excluded_functions: PciExcludedFunctions,
    enumeration_started: bool,
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

    /// Install a pre-scan exclusion snapshot. This is not DMA quiescence or
    /// address reservation: callers MUST reserve all live resources through
    /// fixed BARs or explicit resource reservations before allocation.
    pub fn exclude_functions(&mut self, addresses: &[PciAddress]) -> Result<(), PciEcamError> {
        if self.enumeration_started {
            return Err(PciEcamError::ConfigError);
        }
        let mut next = self.excluded_functions.clone();
        for address in addresses {
            if self.root_of(*address).is_none() || next.contains(address) {
                return Err(PciEcamError::ConfigError);
            }
            let device = self.device(*address).ok_or(PciEcamError::ConfigError)?;
            // Excluding bridges would also need bus-number ownership for
            // their unenumerated descendants; that is not this contract.
            if device.is_present() && device.read8(PCI_HEADER_TYPE) & 0x7f != 0 {
                return Err(PciEcamError::ConfigError);
            }
            next.push(*address).map_err(|_| PciEcamError::ConfigError)?;
        }
        self.excluded_functions = next;
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
                let excluded_zero = self.excluded_functions.contains(&addr);
                if !excluded_zero && self.read32(addr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                    continue;
                }
                let device = self.device(addr).ok_or(PciEcamError::ConfigError)?;
                if excluded_zero || device.read8(PCI_HEADER_TYPE) & PCI_HEADER_TYPE_MULTI_FUNC != 0
                {
                    8
                } else {
                    1
                }
            };

            for func in 0..max_func {
                let faddr = PciAddress::new(segment, bus, dev, func);
                if self.excluded_functions.contains(&faddr) {
                    continue;
                }
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
    // No final BAR, window or command value is programmed until every root
    // is placed: an allocation that fails leaves them as discovery left
    // them. Discovery is not undone: it numbered the bridges' buses and left
    // them so; to size BARs and probe prefetchable windows it wrote them
    // with decode off, and put each back. A failure is fail-stop: the
    // hierarchy is left routed for a diagnosis, not for a retry.

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
    /// On failure no final BAR, window or command value is programmed:
    /// discovery's probes of BARs, prefetchable windows and decode are each
    /// put back. But discovery has numbered the bridges' buses and left them
    /// so: treat a failure as fail-stop, not as a hierarchy to hand off or
    /// enumerate again.
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
        self.enumeration_started = true;
        // Validate preserved BAR snapshots before any probe/decode writes.
        for fixed in self
            .fixed_bars
            .iter()
            .filter(|fixed| self.excluded_functions.contains(&fixed.address))
        {
            self.size_bar(fixed.address, usize::from((fixed.register - PCI_BAR0) / 4))?;
        }
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
            !self.excluded_functions.contains(&fixed.address)
                && !self.devices.iter().any(|dev| {
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

    /// The memory BAR at `register` of `address`, as allocation placed it:
    /// `None` for an I/O BAR, the upper half of a 64-bit BAR or a BAR the
    /// allocator did not place.
    pub fn assigned_memory_bar(&self, address: PciAddress, register: u16) -> Option<PciWindow> {
        let device = self.devices.iter().find(|device| device.addr == address)?;
        let bar = device
            .bars
            .iter()
            .find(|bar| bar.reg == register && bar.allocated)?;
        let low = u64::from(self.read32(address, register) & !0xf);
        let base = match bar.bar_type {
            BarType::Memory32 => low,
            BarType::Memory64 => low | u64::from(self.read32(address, register + 4)) << 32,
            BarType::None | BarType::Io => return None,
        };
        Some(PciWindow::mmio(base, bar.size))
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
            excluded_functions: PciExcludedFunctions::new(),
            enumeration_started: false,
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

#[path = "ecam_host/pool.rs"]
mod pool;

#[cfg(test)]
#[path = "ecam_host/tests.rs"]
mod tests;
