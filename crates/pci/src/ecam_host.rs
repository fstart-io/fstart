//! PCI ECAM host bridge driver with bus enumeration and resource allocation.
//!
//! This driver implements a PCIe root complex that uses the Enhanced
//! Configuration Access Mechanism (ECAM) for config-space access.  On
//! `init()` it performs a full bus walk, sizes every BAR, allocates
//! resources from the MMIO/IO windows declared in its config, programs
//! the BARs and bridge forwarding windows, and enables memory/IO decode.
//!
//! The allocation algorithm follows coreboot's key domain-level rules:
//! chipset-owned resources are explicit and removed from free space, and
//! movable BARs are placed largest-alignment-first. Bridge windows are then
//! derived from the resources assigned below each bridge; this remains a
//! simpler topology model than coreboot's full recursive allocator.
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

use crate::{
    ConfigRegionAccess, PCI_BAR0, PCI_CMD_BUS_MASTER, PCI_CMD_IO, PCI_CMD_MEMORY, PCI_COMMAND,
    PCI_HEADER_TYPE, PCI_HEADER_TYPE_BRIDGE, PCI_HEADER_TYPE_CARDBUS, PCI_HEADER_TYPE_MULTI_FUNC,
    PCI_IO_BASE, PCI_MEMORY_BASE, PCI_PREF_BASE_UPPER32, PCI_PREF_LIMIT_UPPER32,
    PCI_PREF_MEMORY_BASE, PCI_PRIMARY_BUS, PCI_VENDOR_ID, PCI_VENDOR_INVALID, PciAddress,
    PciWindow, PciWindowKind,
};
use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the PCI ECAM host bridge.
///
/// All addresses come from the board metadata and describe the fixed platform
/// windows that QEMU / the SoC provides for PCI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PciEcamConfig {
    /// ECAM base address (memory-mapped PCI config space).
    pub ecam_base: u64,
    /// Size of the ECAM region in bytes (256 MB for 256 buses).
    pub ecam_size: u64,
    /// 32-bit MMIO window base for BAR allocation.
    pub mmio32_base: u64,
    /// 32-bit MMIO window size.
    pub mmio32_size: u64,
    /// 64-bit MMIO window base for BAR allocation.
    pub mmio64_base: u64,
    /// 64-bit MMIO window size.
    pub mmio64_size: u64,
    /// PCI I/O port window base (MMIO-mapped on ARM).
    pub pio_base: u64,
    /// PCI I/O port window size.
    pub pio_size: u64,
    /// First bus number in this segment.
    pub bus_start: u8,
    /// Last bus number in this segment.
    pub bus_end: u8,
}

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

/// A sized but not yet allocated BAR.
#[derive(Debug, Clone, Copy)]
struct BarInfo {
    bar_type: BarType,
    size: u64,
    prefetchable: bool,
    /// BAR register offset (0x10..0x24).
    reg: u16,
    /// Whether this BAR has been successfully assigned by the allocator.
    allocated: bool,
    /// The chipset owns this BAR's address; generic allocation must preserve it.
    fixed: bool,
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

/// Bus (lowest first), needs the low window, alignment: see
/// [`PciEcam::bar_allocation_rank`].
type BarRank = (core::cmp::Reverse<u8>, u8, u64);

impl ResourcePool {
    const fn empty() -> Self {
        Self {
            ranges: [EMPTY_RESOURCE_RANGE; MAX_RESOURCE_RANGES],
            count: 0,
        }
    }

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

    fn allocate_aligned(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || align == 0 {
            return None;
        }
        for idx in 0..self.count {
            let range = self.ranges[idx];
            let aligned = range.base.checked_add(align - 1)? & !(align - 1);
            let end = aligned.checked_add(size)?;
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

/// Maximum number of address windows a single root bridge can have.
///
/// Three is typical (low MMIO, high MMIO, I/O) but a few extra slots
/// accommodate unusual platforms.
const MAX_WINDOWS: usize = crate::MAX_PCI_ROOT_WINDOWS;
const MAX_PCI_DEVICES: usize = 128;

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
    mmio32: ResourcePool,
    mmio64: ResourcePool,
    io_pool: ResourcePool,
    /// Remaining allocatable address windows after fixed reservations.
    windows: [PciWindow; MAX_WINDOWS],
    window_count: usize,
    /// Existing routes through this bus belong to a different owner.
    reserved_through: u8,
    /// Next bus number to assign to a bridge.
    next_bus: u16,
}

impl Root {
    fn contains(&self, addr: PciAddress) -> bool {
        addr.segment() == self.segment && (self.bus_start..=self.bus_end).contains(&addr.bus())
    }

    /// Rebuild the external `windows` array from the current resource pools.
    fn rebuild_windows(&mut self) {
        let pools = [
            (&self.mmio32, PciWindowKind::Mmio, false),
            (&self.mmio64, PciWindowKind::Mmio, true),
            (&self.io_pool, PciWindowKind::Io, false),
        ];
        let mut windows = pools.into_iter().flat_map(|(pool, kind, prefetchable)| {
            pool.ranges[..pool.count]
                .iter()
                .map(move |range| PciWindow {
                    kind,
                    base: range.base,
                    size: range.end - range.base,
                    prefetchable,
                })
        });
        let mut count = 0;
        for (slot, window) in self.windows.iter_mut().zip(&mut windows) {
            *slot = window;
            count += 1;
        }
        self.window_count = count;
    }
}

/// The most root bridges one allocator owns.
pub const MAX_PCI_ROOTS: usize = 8;

/// PCI ECAM host bridge driver.
pub struct PciEcam {
    /// At least one; the bus ranges of a segment don't overlap.
    roots: HVec<Root, MAX_PCI_ROOTS>,
    devices: HVec<PciDev, MAX_PCI_DEVICES>,
    fixed_bars: PciFixedBars,
    auto_bus_master: bool,
}

// SAFETY: MMIO registers are hardware-fixed addresses from the board metadata.
// The driver is used single-threaded during firmware init.
unsafe impl Send for PciEcam {}
unsafe impl Sync for PciEcam {}

impl PciEcam {
    // -- Window management (public for composition by platform drivers) --

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

    /// Replace the resource pools and rebuild the external window list.
    ///
    /// Platform host-bridge drivers (e.g., Q35) use this to set MMIO/IO
    /// windows computed at runtime from hardware state (TOLUD, e820).
    /// Must be called **before** `init()`, on a single root bridge.
    pub fn configure_windows(
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
        root.rebuild_windows();
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

        Ok(Some(PciDev {
            addr,
            root: self.root_of(addr).ok_or(PciEcamError::ConfigError)?,
            header_type,
            original_command: command,
            bars,
            secondary_bus: 0,
            subordinate_bus: 0,
        }))
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

    /// Not on its root bus.
    fn behind_bridge(&self, dev_idx: usize) -> bool {
        let device = &self.devices[dev_idx];
        device.addr.bus() != self.roots[device.root].bus_start
    }

    /// Whether the function's BARs belong to `pass`: 0 for the root bus,
    /// 1 behind a bridge. A bridge's own BARs (a switch upstream port's
    /// registers) decode on its primary bus, inside its parent's window.
    fn bars_in_pass(&self, dev_idx: usize, pass: usize) -> bool {
        let behind_bridge = self.behind_bridge(dev_idx);
        (pass == 0 && !behind_bridge) || (pass == 1 && behind_bridge)
    }

    fn bar_allocation_alignment(&self, dev_idx: usize, bar_idx: usize) -> u64 {
        let bar = self.devices[dev_idx].bars[bar_idx];
        let behind_bridge = self.behind_bridge(dev_idx);
        match bar.bar_type {
            BarType::Memory32 | BarType::Memory64 => {
                if behind_bridge {
                    bar.size.max(0x100000)
                } else {
                    bar.size
                }
            }
            BarType::Io => {
                if behind_bridge {
                    bar.size.max(0x1000)
                } else {
                    bar.size
                }
            }
            BarType::None => 0,
        }
    }

    fn next_bar_to_allocate(&self, pass: usize) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, BarRank)> = None;
        for dev_idx in 0..self.devices.len() {
            if !self.bars_in_pass(dev_idx, pass) {
                continue;
            }
            for bar_idx in 0..6 {
                let bar = self.devices[dev_idx].bars[bar_idx];
                if bar.bar_type == BarType::None || bar.fixed || bar.allocated {
                    continue;
                }
                let rank = self.bar_allocation_rank(dev_idx, bar_idx);
                if best.is_none_or(|(_, _, best_rank)| rank > best_rank) {
                    best = Some((dev_idx, bar_idx, rank));
                }
            }
        }
        best.map(|(dev_idx, bar_idx, _)| (dev_idx, bar_idx))
    }

    /// Allocation rank, highest first.
    ///
    /// Bus by bus: bus numbers are assigned depth first, so every bridge's
    /// subtree ends up in one contiguous span and the windows derived from
    /// those spans can't overlap a sibling's. On a bus, BARs that only fit
    /// below 4 GiB go first, then the largest alignment.
    fn bar_allocation_rank(&self, dev_idx: usize, bar_idx: usize) -> BarRank {
        let needs_low_window = self.devices[dev_idx].bars[bar_idx].bar_type == BarType::Memory32;
        (
            core::cmp::Reverse(self.devices[dev_idx].addr.bus()),
            u8::from(needs_low_window),
            self.bar_allocation_alignment(dev_idx, bar_idx),
        )
    }

    /// Below-4-GiB bytes the unallocated 32-bit memory BARs of root `root`
    /// still need.
    fn pending_low_bytes(&self, root: usize) -> u64 {
        (0..self.devices.len())
            .filter(|&dev_idx| {
                let device = &self.devices[dev_idx];
                device.root == root && device.header_type != PCI_HEADER_TYPE_BRIDGE
            })
            .flat_map(|dev_idx| (0..6).map(move |bar_idx| (dev_idx, bar_idx)))
            .filter(|&(dev_idx, bar_idx)| {
                let bar = self.devices[dev_idx].bars[bar_idx];
                bar.bar_type == BarType::Memory32 && !bar.fixed && !bar.allocated
            })
            .map(|(dev_idx, bar_idx)| self.bar_allocation_alignment(dev_idx, bar_idx))
            .sum()
    }

    fn allocate_one_bar(&mut self, dev_idx: usize, bar_idx: usize) -> Result<(), PciEcamError> {
        let addr = self.devices[dev_idx].addr;
        let root = self.devices[dev_idx].root;
        let bar = self.devices[dev_idx].bars[bar_idx];
        let align = self.bar_allocation_alignment(dev_idx, bar_idx);
        let behind_bridge = self.behind_bridge(dev_idx);
        let pending = self.pending_low_bytes(root);
        let pools = &mut self.roots[root];
        let base = match bar.bar_type {
            BarType::Memory32 => pools.mmio32.allocate_aligned(bar.size, align),
            // The below-4-GiB window comes first: firmware drivers reach a BAR
            // through a mapping that does not cover above 4 GiB, and some
            // devices that declare a 64-bit BAR still do not decode that high.
            // The above-4-GiB window takes what does not fit, and what would
            // strand a 32-bit BAR that has no other home.
            // A bridge forwards non-prefetchable memory only through its
            // 32-bit window: behind one, such a BAR has no home above 4 GiB.
            BarType::Memory64 if !bar.prefetchable && behind_bridge => {
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

        let base = base
            .ok_or(PciEcamError::ResourceExhausted)
            .inspect_err(|error| {
                error.log_bar("BAR allocation", addr, bar.reg);
                fstart_log::error!(
                    "pci: requested BAR size={:#x} alignment={:#x} type={}",
                    bar.size,
                    align,
                    match bar.bar_type {
                        BarType::Memory32 => "memory32",
                        BarType::Memory64 => "memory64",
                        BarType::Io => "I/O",
                        BarType::None => "none",
                    }
                );
            })?;
        self.write16(
            addr,
            PCI_COMMAND,
            self.devices[dev_idx].original_command & !(PCI_CMD_IO | PCI_CMD_MEMORY),
        );
        match bar.bar_type {
            BarType::Memory32 => {
                let val = (base as u32 & 0xFFFF_FFF0) | if bar.prefetchable { 0x8 } else { 0 };
                self.write32(addr, bar.reg, val);
            }
            BarType::Memory64 => {
                let lo = (base as u32 & 0xFFFF_FFF0) | 0x4 | if bar.prefetchable { 0x8 } else { 0 };
                self.write32(addr, bar.reg, lo);
                self.write32(addr, bar.reg + 4, (base >> 32) as u32);
            }
            BarType::Io => {
                self.write32(addr, bar.reg, (base as u32) | 0x1);
            }
            BarType::None => {}
        }
        self.write16(addr, PCI_COMMAND, self.devices[dev_idx].original_command);

        self.devices[dev_idx].bars[bar_idx].allocated = true;
        Ok(())
    }

    /// Allocate and program BARs for all non-bridge devices, then program
    /// bridge forwarding windows.
    fn allocate_resources(&mut self) -> Result<(), PciEcamError> {
        // Phase 1: allocate endpoint BARs. Within each topology pass, allocate
        // largest-alignment BARs first. This avoids consuming the front of a
        // constrained 32-bit aperture with a small BAR, then aligning a large
        // framebuffer BAR up and stranding the remaining space below it.
        for pass in 0..2 {
            while let Some((dev_idx, bar_idx)) = self.next_bar_to_allocate(pass) {
                self.allocate_one_bar(dev_idx, bar_idx)?;
            }
        }

        for dev in &self.devices {
            if dev.header_type == PCI_HEADER_TYPE_BRIDGE {
                continue;
            }
            let has_io = dev
                .bars
                .iter()
                .any(|bar| bar.allocated && bar.bar_type == BarType::Io);
            let has_memory = dev.bars.iter().any(|bar| {
                bar.allocated && matches!(bar.bar_type, BarType::Memory32 | BarType::Memory64)
            });
            // BAR probing temporarily disabled decode. Preserve chipset-owned
            // and subtractive resources represented by the original command
            // policy, then add decode required by newly assigned BARs.
            let mut new_cmd = dev.original_command;
            if has_io {
                new_cmd |= PCI_CMD_IO;
            }
            if has_memory {
                new_cmd |= PCI_CMD_MEMORY;
            }
            if self.auto_bus_master {
                new_cmd |= PCI_CMD_BUS_MASTER;
            } else {
                new_cmd &= !PCI_CMD_BUS_MASTER;
            }
            self.write16(dev.addr, PCI_COMMAND, new_cmd);
        }

        // Phase 2: program bridge forwarding windows.
        for i in 0..self.devices.len() {
            if self.devices[i].header_type != PCI_HEADER_TYPE_BRIDGE {
                continue;
            }

            let baddr = self.devices[i].addr;
            let root = self.devices[i].root;
            let sec = self.devices[i].secondary_bus;
            let sub = self.devices[i].subordinate_bus;

            // Compute the span of addresses used by children behind this bridge.
            // The prefetchable window is 64-bit, the other one 32-bit only:
            // when any prefetchable BAR behind the bridge lies above 4 GiB, the
            // prefetchable window covers exactly those, and prefetchable BARs
            // below 4 GiB go through the non-prefetchable window (prefetchable
            // memory may always be forwarded as non-prefetchable). Otherwise a
            // window spanning both regions would claim the fixed apertures and
            // DRAM in between.
            let mut mem_lo: u64 = u64::MAX;
            let mut mem_hi: u64 = 0;
            let mut pref_lo: u64 = u64::MAX;
            let mut pref_hi: u64 = 0;
            let mut io_lo: u64 = u64::MAX;
            let mut io_hi: u64 = 0;
            let high_pref = self.devices.iter().any(|child| {
                child.root == root
                    && (sec..=sub).contains(&child.addr.bus())
                    && child.bars.iter().any(|bar| {
                        bar.prefetchable
                            && bar.bar_type == BarType::Memory64
                            && self.read32(child.addr, bar.reg + 4) != 0
                    })
            });

            for child in &self.devices {
                if child.root != root || child.addr.bus() < sec || child.addr.bus() > sub {
                    continue;
                }
                for bar in &child.bars {
                    if bar.bar_type == BarType::None {
                        continue;
                    }
                    if bar.bar_type == BarType::Io {
                        let base = (self.read32(child.addr, bar.reg) & 0x0000_FFFC) as u64;
                        if base != 0 {
                            io_lo = io_lo.min(base);
                            io_hi = io_hi.max(base + bar.size);
                        }
                        continue;
                    }
                    // Read back the programmed base.
                    let base_lo = self.read32(child.addr, bar.reg) & 0xFFFF_FFF0;
                    let base = if bar.bar_type == BarType::Memory64 {
                        let hi = self.read32(child.addr, bar.reg + 4);
                        ((hi as u64) << 32) | (base_lo as u64)
                    } else {
                        base_lo as u64
                    };
                    if base == 0 {
                        continue;
                    }
                    let end = base + bar.size;

                    if bar.prefetchable && (!high_pref || base >= 1 << 32) {
                        pref_lo = pref_lo.min(base);
                        pref_hi = pref_hi.max(end);
                    } else {
                        mem_lo = mem_lo.min(base);
                        mem_hi = mem_hi.max(end);
                    }
                }
            }

            // Non-prefetchable memory window (base/limit in 1 MiB granularity).
            if mem_lo < mem_hi {
                let base_reg = ((mem_lo >> 16) & 0xFFF0) as u16;
                let limit_reg = (((mem_hi - 1) >> 16) & 0xFFF0) as u16;
                self.write32(
                    baddr,
                    PCI_MEMORY_BASE,
                    (base_reg as u32) | ((limit_reg as u32) << 16),
                );
            } else {
                // Disable: base > limit.
                self.write32(baddr, PCI_MEMORY_BASE, 0x0000_FFFF);
            }

            // Prefetchable memory window (64-bit capable).
            if pref_lo < pref_hi {
                let base_reg = ((pref_lo >> 16) & 0xFFF0) as u16;
                let limit_reg = (((pref_hi - 1) >> 16) & 0xFFF0) as u16;
                self.write32(
                    baddr,
                    PCI_PREF_MEMORY_BASE,
                    (base_reg as u32) | ((limit_reg as u32) << 16),
                );
                self.write32(baddr, PCI_PREF_BASE_UPPER32, (pref_lo >> 32) as u32);
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, ((pref_hi - 1) >> 32) as u32);
            } else {
                self.write32(baddr, PCI_PREF_MEMORY_BASE, 0x0000_FFFF);
                self.write32(baddr, PCI_PREF_BASE_UPPER32, 0);
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, 0);
            }

            if io_lo < io_hi {
                let base = ((io_lo >> 8) & 0xF0) as u8;
                let limit = (((io_hi - 1) >> 8) & 0xF0) as u8;
                self.write32(baddr, PCI_IO_BASE, (base as u32) | ((limit as u32) << 8));
            } else {
                self.write32(baddr, PCI_IO_BASE, 0x00FF);
            }

            // Enable forwarding, but defer mastering when DMA ownership is
            // explicitly left to the device driver.
            let mut new_cmd = self.devices[i].original_command | PCI_CMD_IO | PCI_CMD_MEMORY;
            if self.auto_bus_master {
                new_cmd |= PCI_CMD_BUS_MASTER;
            } else {
                new_cmd &= !PCI_CMD_BUS_MASTER;
            }
            self.write16(baddr, PCI_COMMAND, new_cmd);
        }
        Ok(())
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

    /// Memory BAR spans actually assigned to discovered devices, including
    /// chipset-fixed BARs. I/O BARs and unassigned/probed slots are excluded.
    pub fn assigned_memory_windows(&self) -> impl Iterator<Item = PciWindow> + '_ {
        self.assigned_memory_bars().map(|(_, window)| window)
    }

    /// Allocated prefetchable VGA apertures eligible for write combining.
    /// Non-prefetchable GPU registers and bridge windows are never included.
    pub fn prefetchable_vga_windows(&self) -> impl Iterator<Item = PciWindow> + '_ {
        self.assigned_memory_bars().filter_map(|(address, window)| {
            (window.prefetchable
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
        self.devices.iter().flat_map(move |device| {
            device.bars.iter().filter_map(move |bar| {
                if !bar.allocated {
                    return None;
                }
                let low = self.read32(device.addr, bar.reg);
                let (kind, base) = match bar.bar_type {
                    BarType::None => return None,
                    BarType::Io => (PciWindowKind::Io, u64::from(low & !3)),
                    BarType::Memory32 => (PciWindowKind::Mmio, u64::from(low & !0xf)),
                    BarType::Memory64 => (
                        PciWindowKind::Mmio,
                        u64::from(low & !0xf)
                            | (u64::from(self.read32(device.addr, bar.reg + 4)) << 32),
                    ),
                };
                Some((
                    device.addr,
                    PciWindow {
                        kind,
                        base,
                        size: bar.size,
                        prefetchable: bar.prefetchable,
                    },
                ))
            })
        })
    }

    /// Construct an ECAM root bridge from a runtime provider.
    ///
    /// The provider is queried once, after memory discovery, and the returned
    /// allocation windows are copied into the allocator as a stable snapshot.
    pub fn from_provider<P>(provider: &P) -> Result<Self, PciEcamError>
    where
        P: crate::PciRootProvider + ?Sized,
    {
        Self::from_providers([provider])
    }

    /// Construct an allocator for several root bridges, each with its own
    /// buses and windows, as [`Self::from_provider`]. The bus ranges of a
    /// segment must not overlap.
    pub fn from_providers<'a, P>(
        providers: impl IntoIterator<Item = &'a P>,
    ) -> Result<Self, PciEcamError>
    where
        P: crate::PciRootProvider + ?Sized + 'a,
    {
        let roots = providers.into_iter().map(Root::from_provider).try_fold(
            HVec::new(),
            |mut roots: HVec<Root, MAX_PCI_ROOTS>, root| {
                let root = root?;
                let overlaps = roots.iter().any(|other| {
                    other.segment == root.segment
                        && other.bus_start <= root.bus_end
                        && root.bus_start <= other.bus_end
                });
                if overlaps || roots.push(root).is_err() {
                    return Err(PciEcamError::ConfigError);
                }
                Ok(roots)
            },
        )?;
        if roots.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        Ok(Self::new(roots))
    }

    fn new(roots: HVec<Root, MAX_PCI_ROOTS>) -> Self {
        Self {
            roots,
            devices: HVec::new(),
            fixed_bars: PciFixedBars::new(),
            auto_bus_master: true,
        }
    }

    /// Construct an ECAM root bridge from a temporary config.
    ///
    /// `PciEcam` copies only scalar window values into runtime pools and does
    /// not retain a reference to the config, so composed host bridges can build
    /// this from hardware-derived local values without cloning a board config.
    pub fn from_config(config: &PciEcamConfig) -> Result<Self, PciEcamError> {
        if config.bus_end < config.bus_start {
            return Err(PciEcamError::ConfigError);
        }

        // Build the window list from the config.  Only add windows that
        // have a non-zero size (the platform may omit some).
        let dummy = PciWindow {
            kind: PciWindowKind::Mmio,
            base: 0,
            size: 0,
            prefetchable: false,
        };
        let mut windows = [dummy; MAX_WINDOWS];
        let mut wc = 0;

        let (mmio32_base, mmio32_size) = (config.mmio32_base, config.mmio32_size);

        if mmio32_size > 0 {
            windows[wc] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: mmio32_base,
                size: mmio32_size,
                prefetchable: false,
            };
            wc += 1;
        }
        if config.mmio64_size > 0 {
            windows[wc] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: config.mmio64_base,
                size: config.mmio64_size,
                // The high MMIO window is typically used for prefetchable
                // 64-bit BARs (framebuffers, NVMe, etc.).  Mark it
                // prefetchable so ACPI _CRS descriptors are correct.
                prefetchable: true,
            };
            wc += 1;
        }
        if config.pio_size > 0 {
            windows[wc] = PciWindow {
                kind: PciWindowKind::Io,
                base: config.pio_base,
                size: config.pio_size,
                prefetchable: false,
            };
            wc += 1;
        }

        let root = Root {
            segment: 0,
            ecam_base: config.ecam_base as usize,
            ecam_size: config.ecam_size as usize,
            bus_start: config.bus_start,
            bus_end: config.bus_end,
            mmio32: ResourcePool::new(mmio32_base, mmio32_size)?,
            mmio64: ResourcePool::new(config.mmio64_base, config.mmio64_size)?,
            io_pool: ResourcePool::new(config.pio_base, config.pio_size)?,
            windows,
            window_count: wc,
            reserved_through: config.bus_start,
            next_bus: u16::from(config.bus_start) + 1,
        };
        Ok(Self::new(HVec::from_iter([root])))
    }
}

impl Root {
    fn from_provider<P>(provider: &P) -> Result<Self, PciEcamError>
    where
        P: crate::PciRootProvider + ?Sized,
    {
        let info = provider.root_info();
        if info.bus_end < info.bus_start {
            return Err(PciEcamError::ConfigError);
        }

        let supplied = provider
            .resource_windows()
            .map_err(|_| PciEcamError::ConfigError)?;
        let dummy = PciWindow {
            kind: PciWindowKind::Mmio,
            base: 0,
            size: 0,
            prefetchable: false,
        };
        let mut windows = [dummy; MAX_WINDOWS];
        let mut window_count = 0;
        let mut mmio32 = ResourcePool::empty();
        let mut mmio64 = ResourcePool::empty();
        let mut io_pool = ResourcePool::empty();

        for window in supplied {
            if window.size == 0 {
                continue;
            }
            let Some(end) = window.base.checked_add(window.size) else {
                return Err(PciEcamError::ConfigError);
            };
            if window_count == MAX_WINDOWS {
                return Err(PciEcamError::ConfigError);
            }

            match window.kind {
                PciWindowKind::Io => io_pool.push_range(window.base, end)?,
                PciWindowKind::Mmio if window.is_below_4g() => {
                    mmio32.push_range(window.base, end)?;
                }
                PciWindowKind::Mmio => mmio64.push_range(window.base, end)?,
            }
            windows[window_count] = window;
            window_count += 1;
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
            windows,
            window_count,
            reserved_through: info.bus_start,
            next_bus: u16::from(info.bus_start) + 1,
        })
    }
}

/// The ECAM and window accessors describe the first root bridge: all of it
/// on a single-root platform.
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

    pub fn windows(&self) -> &[PciWindow] {
        let root = &self.roots[0];
        &root.windows[..root.window_count]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PciRootError, PciRootInfo, PciRootProvider, PciRootWindows};
    use alloc::vec;
    use alloc::vec::Vec;

    fn test_pci() -> (PciEcam, Vec<u32>) {
        const BUSES: usize = 3;
        let mut config_space = vec![0u32; BUSES << 18];
        let ecam_base = config_space.as_mut_ptr() as usize;
        let dummy = PciWindow {
            kind: PciWindowKind::Io,
            base: 0,
            size: 0,
            prefetchable: false,
        };
        let root = Root {
            segment: 0,
            ecam_base,
            ecam_size: BUSES << 20,
            bus_start: 0,
            bus_end: BUSES as u8 - 1,
            mmio32: ResourcePool::new(0x8000_0000, 0x1000_0000).unwrap(),
            mmio64: ResourcePool::empty(),
            io_pool: ResourcePool::new(0x1000, 0xf000).unwrap(),
            windows: [dummy; MAX_WINDOWS],
            window_count: 0,
            reserved_through: 0,
            next_bus: 1,
        };
        (PciEcam::new(HVec::from_iter([root])), config_space)
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

    fn endpoint(addr: PciAddress, definitions: &[(usize, BarType, u64)]) -> PciDev {
        let none = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg: 0,
            allocated: false,
            fixed: false,
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
        }
    }

    fn io_bar_base(pci: &PciEcam, addr: PciAddress, index: usize) -> u64 {
        u64::from(pci.read32(addr, PCI_BAR0 + index as u16 * 4) & 0x0000_fffc)
    }

    struct TestRoot;

    impl PciRootProvider for TestRoot {
        fn root_info(&self) -> PciRootInfo {
            PciRootInfo {
                segment: 3,
                ecam_base: 0xe000_0000,
                bus_start: 0x40,
                bus_end: 0x7f,
            }
        }

        fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
            let mut windows = PciRootWindows::new();
            windows
                .push(PciWindow {
                    kind: PciWindowKind::Mmio,
                    base: 0x8000_0000,
                    size: 0x1000_0000,
                    prefetchable: false,
                })
                .map_err(|_| PciRootError::TooManyWindows)?;
            windows
                .push(PciWindow {
                    kind: PciWindowKind::Io,
                    base: 0x1000,
                    size: 0xf000,
                    prefetchable: false,
                })
                .map_err(|_| PciRootError::TooManyWindows)?;
            Ok(windows)
        }
    }

    #[test]
    fn provider_is_snapshotted_into_ecam_allocator() {
        let pci = PciEcam::from_provider(&TestRoot).unwrap();

        assert_eq!(pci.roots[0].segment, 3);
        assert_eq!(pci.ecam_base(), 0xe000_0000);
        assert_eq!(pci.ecam_size(), 64 * 1024 * 1024);
        assert_eq!(pci.bus_start(), 0x40);
        assert_eq!(pci.bus_end(), 0x7f);
        assert_eq!(pci.windows().len(), 2);
        assert_eq!(pci.windows()[0].kind, PciWindowKind::Mmio);
        assert_eq!(pci.windows()[1].kind, PciWindowKind::Io);
    }

    /// A root bridge of fixed buses with one MMIO window.
    struct Domain(PciRootInfo, u64);
    impl PciRootProvider for Domain {
        fn root_info(&self) -> PciRootInfo {
            self.0
        }
        fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
            Ok(PciRootWindows::from_iter([PciWindow {
                kind: PciWindowKind::Mmio,
                base: self.1,
                size: 0x1000_0000,
                prefetchable: false,
            }]))
        }
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
        let domain = |bus_start, bus_end, window| {
            let info = PciRootInfo {
                segment: 0,
                ecam_base: base as u64,
                bus_start,
                bus_end,
            };
            Domain(info, window)
        };
        let roots = [
            domain(0, 0x3f, 0x8000_0000),
            domain(0x40, 0x7f, 0xa000_0000),
        ];
        assert!(matches!(
            PciEcam::from_providers(&[domain(0, 0x40, 0), domain(0x40, 0x7f, 0)]),
            Err(PciEcamError::ConfigError)
        ));
        let mut pci = PciEcam::from_providers(&roots).unwrap();

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
        let mut pci = PciEcam::from_provider(&Domain(info, 0x8000_0000)).unwrap();
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
        // Consumed child resources do not shrink the host's parent aperture.
        assert!(
            pci.windows()
                .iter()
                .any(|window| window.kind == PciWindowKind::Io
                    && window.base == 0x15c0
                    && window.size == 0x200)
        );
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
        assert_eq!(pci.windows().len(), 0);
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
