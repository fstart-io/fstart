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
//! Compatible: `"pci-host-ecam-generic"`.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciEcamError {
    ConfigError,
    ResourceRangeLimit,
    FixedBarInvalid,
    FixedBarConflict,
    ResourceExhausted,
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
    header_type: u8,
    /// Command register before BAR probing temporarily disabled decode.
    original_command: u16,
    bars: [BarInfo; 6],
    /// For bridges: secondary bus number.
    secondary_bus: u8,
    /// For bridges: subordinate bus number.
    subordinate_bus: u8,
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
/// chipset resources such as ECAM. This matches coreboot's domain allocator,
/// which subtracts fixed resources before placing dynamic BARs.
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
            self.ranges[idx].base = end;
            if self.ranges[idx].base == self.ranges[idx].end {
                for next in idx + 1..self.count {
                    self.ranges[next - 1] = self.ranges[next];
                }
                self.count -= 1;
            }
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

/// PCI ECAM host bridge driver.
pub struct PciEcam {
    segment: u16,
    ecam_base: usize,
    ecam_size: usize,
    bus_start: u8,
    bus_end: u8,
    mmio32: ResourcePool,
    mmio64: ResourcePool,
    io_pool: ResourcePool,
    /// Remaining allocatable address windows after fixed reservations.
    windows: [PciWindow; MAX_WINDOWS],
    window_count: usize,
    devices: HVec<PciDev, MAX_PCI_DEVICES>,
    fixed_bars: PciFixedBars,
    /// Next bus number to assign to a bridge.
    next_bus: u8,
}

// SAFETY: MMIO registers are hardware-fixed addresses from the board metadata.
// The driver is used single-threaded during firmware init.
unsafe impl Send for PciEcam {}
unsafe impl Sync for PciEcam {}

impl PciEcam {
    // -- Window management (public for composition by platform drivers) --

    /// Replace the resource pools and rebuild the external window list.
    ///
    /// Platform host-bridge drivers (e.g., Q35) use this to set MMIO/IO
    /// windows computed at runtime from hardware state (TOLUD, e820).
    /// Must be called **before** `init()`.
    pub fn configure_windows(
        &mut self,
        mmio32_base: u64,
        mmio32_size: u64,
        mmio64_base: u64,
        mmio64_size: u64,
        pio_base: u64,
        pio_size: u64,
    ) -> Result<(), PciEcamError> {
        if !self.fixed_bars.is_empty() {
            return Err(PciEcamError::ConfigError);
        }
        self.mmio32 = ResourcePool::new(mmio32_base, mmio32_size)?;
        self.mmio64 = ResourcePool::new(mmio64_base, mmio64_size)?;
        self.io_pool = ResourcePool::new(pio_base, pio_size)?;
        self.rebuild_windows();
        Ok(())
    }

    /// Reserve a fixed MMIO range before enumeration.
    ///
    /// The range is removed from both 32-bit and 64-bit allocation pools so
    /// platform-fixed resources cannot be overlapped by dynamic BARs.
    pub fn reserve_mmio_range(&mut self, base: u64, size: u64) -> Result<(), PciEcamError> {
        let mut mmio32 = self.mmio32;
        let mut mmio64 = self.mmio64;
        mmio32.reserve_range(base, size)?;
        mmio64.reserve_range(base, size)?;
        self.mmio32 = mmio32;
        self.mmio64 = mmio64;
        Ok(())
    }

    /// Register chipset-owned BARs before enumeration.
    ///
    /// Fixed BARs remain visible in the root aperture reported to the OS, but
    /// their intervals are removed from the allocator's private free pools.
    pub fn add_fixed_bars(&mut self, bars: &[PciFixedBar]) -> Result<(), PciEcamError> {
        let mut mmio32 = self.mmio32;
        let mut mmio64 = self.mmio64;
        let mut io_pool = self.io_pool;
        let mut fixed_bars = self.fixed_bars.clone();

        for bar in bars {
            self.validate_fixed_bar(bar)?;
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

        self.mmio32 = mmio32;
        self.mmio64 = mmio64;
        self.io_pool = io_pool;
        self.fixed_bars = fixed_bars;
        Ok(())
    }

    fn validate_fixed_bar(&self, bar: &PciFixedBar) -> Result<(), PciEcamError> {
        let end = bar
            .base
            .checked_add(bar.size)
            .ok_or(PciEcamError::FixedBarInvalid)?;
        if bar.address.segment() != self.segment
            || bar.address.bus() < self.bus_start
            || bar.address.bus() > self.bus_end
            || !(PCI_BAR0..=PCI_BAR0 + 5 * 4).contains(&bar.register)
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
        Ok(())
    }

    /// Rebuild the external `windows` array from the current resource pools.
    fn rebuild_windows(&mut self) {
        self.window_count = 0;

        for range in self.mmio32.ranges[..self.mmio32.count].iter().copied() {
            if self.window_count == MAX_WINDOWS {
                break;
            }
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: range.base,
                size: range.end - range.base,
                prefetchable: false,
            };
            self.window_count += 1;
        }
        for range in self.mmio64.ranges[..self.mmio64.count].iter().copied() {
            if self.window_count == MAX_WINDOWS {
                break;
            }
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: range.base,
                size: range.end - range.base,
                prefetchable: true,
            };
            self.window_count += 1;
        }
        for range in self.io_pool.ranges[..self.io_pool.count].iter().copied() {
            if self.window_count == MAX_WINDOWS {
                break;
            }
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Io,
                base: range.base,
                size: range.end - range.base,
                prefetchable: false,
            };
            self.window_count += 1;
        }
    }

    // -- ECAM helpers --

    fn ecam_addr(&self, addr: PciAddress, reg: u16) -> Option<usize> {
        if addr.segment() != self.segment
            || addr.bus() < self.bus_start
            || addr.bus() > self.bus_end
        {
            return None;
        }
        let offset = ((addr.bus() as usize) << 20)
            | ((addr.device() as usize) << 15)
            | ((addr.function() as usize) << 12)
            | ((reg as usize) & 0xFFC);
        if offset < self.ecam_size {
            Some(self.ecam_base + offset)
        } else {
            None
        }
    }

    fn read32(&self, addr: PciAddress, reg: u16) -> u32 {
        match self.ecam_addr(addr, reg) {
            // SAFETY: ECAM region is memory-mapped PCI config space.
            Some(a) => unsafe { fstart_core::mmio::read32(a as *const u32) },
            None => 0xFFFF_FFFF,
        }
    }

    fn write32(&self, addr: PciAddress, reg: u16, val: u32) {
        if let Some(a) = self.ecam_addr(addr, reg) {
            // SAFETY: ECAM region is memory-mapped PCI config space.
            unsafe { fstart_core::mmio::write32(a as *mut u32, val) };
        }
    }

    fn read16(&self, addr: PciAddress, reg: u16) -> u16 {
        match self.ecam_addr(addr, reg) {
            // SAFETY: the requested halfword is within the mapped config dword.
            Some(a) => unsafe {
                fstart_core::mmio::read16((a + usize::from(reg & 2)) as *const u16)
            },
            None => u16::MAX,
        }
    }

    fn write16(&self, addr: PciAddress, reg: u16, val: u16) {
        if let Some(a) = self.ecam_addr(addr, reg) {
            // SAFETY: the requested halfword is within the mapped config dword.
            unsafe {
                fstart_core::mmio::write16((a + usize::from(reg & 2)) as *mut u16, val);
            }
        }
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

        if moving_lo == 0 {
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
    fn probe_device(&self, addr: PciAddress) -> Result<Option<PciDev>, PciEcamError> {
        let vendor_device = self.read32(addr, PCI_VENDOR_ID);
        if vendor_device == PCI_VENDOR_INVALID {
            return Ok(None);
        }
        let hdr = self.read32(addr, PCI_HEADER_TYPE);
        let header_type = (hdr >> 16) as u8 & 0x7F;
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
        self.write16(addr, PCI_COMMAND, command & !(PCI_CMD_IO | PCI_CMD_MEMORY));

        let sizing_result = (|| {
            let mut i = 0;
            while i < max_bars {
                let (info, is_64) = self.size_bar(addr, i)?;
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

        Ok(Some(PciDev {
            addr,
            header_type,
            original_command: command,
            bars,
            secondary_bus: 0,
            subordinate_bus: 0,
        }))
    }

    // -- Enumeration --

    /// Enumerate a bus recursively.  Discovers devices, assigns bus numbers
    /// to bridges, and recurses behind them.
    fn enumerate_bus(&mut self, bus: u8) -> Result<(), PciEcamError> {
        for dev in 0..32u8 {
            let addr = PciAddress::new(self.segment, bus, dev, 0);
            if self.read32(addr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                continue;
            }

            // Check multi-function bit
            let hdr = self.read32(addr, PCI_HEADER_TYPE);
            let multi_func = (hdr >> 16) as u8 & PCI_HEADER_TYPE_MULTI_FUNC;
            let max_func = if multi_func != 0 { 8 } else { 1 };

            for func in 0..max_func {
                let faddr = PciAddress::new(self.segment, bus, dev, func);
                if func > 0 && self.read32(faddr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                    continue;
                }

                if let Some(mut pci_dev) = self.probe_device(faddr)? {
                    match pci_dev.header_type {
                        PCI_HEADER_TYPE_BRIDGE => {
                            let secondary = self.next_bus;
                            self.next_bus = self.next_bus.saturating_add(1);
                            pci_dev.secondary_bus = secondary;

                            // Temporarily set subordinate to max so scanning works.
                            self.write32(
                                faddr,
                                PCI_PRIMARY_BUS,
                                (bus as u32)
                                    | ((secondary as u32) << 8)
                                    | ((self.bus_end as u32) << 16),
                            );

                            self.enumerate_bus(secondary)?;

                            // Finalise subordinate = highest bus found.
                            pci_dev.subordinate_bus = self.next_bus.saturating_sub(1);
                            self.write32(
                                faddr,
                                PCI_PRIMARY_BUS,
                                (bus as u32)
                                    | ((secondary as u32) << 8)
                                    | ((pci_dev.subordinate_bus as u32) << 16),
                            );
                        }
                        PCI_HEADER_TYPE_CARDBUS => {
                            let secondary = self.next_bus;
                            // CardBus bridges need a bus-number range for
                            // cards inserted later. Reserve the conventional
                            // four-bus window used by Linux/coreboot rather
                            // than leaving the bridge at [bus 00-00].
                            let subordinate = secondary.saturating_add(3).min(self.bus_end);
                            self.next_bus = subordinate.saturating_add(1);
                            pci_dev.secondary_bus = secondary;
                            pci_dev.subordinate_bus = subordinate;
                            self.write32(
                                faddr,
                                PCI_PRIMARY_BUS,
                                (bus as u32)
                                    | ((secondary as u32) << 8)
                                    | ((subordinate as u32) << 16),
                            );
                        }
                        _ => {}
                    }

                    if self.devices.push(pci_dev).is_err() {
                        return Err(PciEcamError::ConfigError);
                    }
                }
            }
        }
        Ok(())
    }

    // -- Resource allocation --

    fn endpoint_in_pass(&self, dev_idx: usize, pass: usize) -> bool {
        if self.devices[dev_idx].header_type == PCI_HEADER_TYPE_BRIDGE {
            return false;
        }
        let behind_bridge = self.devices[dev_idx].addr.bus() != self.bus_start;
        (pass == 0 && !behind_bridge) || (pass == 1 && behind_bridge)
    }

    fn bar_allocation_alignment(&self, dev_idx: usize, bar_idx: usize) -> u64 {
        let bar = self.devices[dev_idx].bars[bar_idx];
        let behind_bridge = self.devices[dev_idx].addr.bus() != self.bus_start;
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
        let mut best: Option<(usize, usize, (u8, u64))> = None;
        for dev_idx in 0..self.devices.len() {
            if !self.endpoint_in_pass(dev_idx, pass) {
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
    /// The below-4-GiB window is the constrained resource: firmware drivers
    /// reach a BAR through a mapping that does not cover above 4 GiB, and a
    /// 32-bit BAR has no alternative home, while a 64-bit BAR can fall back
    /// above 4 GiB. So BARs that only fit below 4 GiB are placed first, and
    /// within each group the largest alignment goes first. Spending the low
    /// window on alignment alone would let a large 64-bit BAR strand a 32-bit
    /// BAR with nothing left to allocate from.
    fn bar_allocation_rank(&self, dev_idx: usize, bar_idx: usize) -> (u8, u64) {
        let needs_low_window = self.devices[dev_idx].bars[bar_idx].bar_type == BarType::Memory32;
        (
            u8::from(needs_low_window),
            self.bar_allocation_alignment(dev_idx, bar_idx),
        )
    }

    fn allocate_one_bar(&mut self, dev_idx: usize, bar_idx: usize) -> Result<(), PciEcamError> {
        let addr = self.devices[dev_idx].addr;
        let bar = self.devices[dev_idx].bars[bar_idx];
        let align = self.bar_allocation_alignment(dev_idx, bar_idx);
        let base = match bar.bar_type {
            BarType::Memory32 => self.mmio32.allocate_aligned(bar.size, align),
            // The below-4-GiB window comes first: firmware drivers reach a BAR
            // through a mapping that does not cover above 4 GiB, and some
            // devices that declare a 64-bit BAR still do not decode that high.
            // The above-4-GiB window is the fallback for what does not fit.
            BarType::Memory64 => self
                .mmio32
                .allocate_aligned(bar.size, align)
                .or_else(|| self.mmio64.allocate_aligned(bar.size, align)),
            BarType::Io => self.io_pool.allocate_aligned(bar.size, align),
            BarType::None => None,
        };

        let base = base.ok_or(PciEcamError::ResourceExhausted)?;
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
            new_cmd |= PCI_CMD_BUS_MASTER;
            self.write16(dev.addr, PCI_COMMAND, new_cmd);
        }

        // Phase 2: program bridge forwarding windows.
        for i in 0..self.devices.len() {
            if self.devices[i].header_type != PCI_HEADER_TYPE_BRIDGE {
                continue;
            }

            let baddr = self.devices[i].addr;
            let sec = self.devices[i].secondary_bus;
            let sub = self.devices[i].subordinate_bus;

            // Compute the span of addresses used by children behind this bridge.
            let mut mem_lo: u64 = u64::MAX;
            let mut mem_hi: u64 = 0;
            let mut pref_lo: u64 = u64::MAX;
            let mut pref_hi: u64 = 0;
            let mut io_lo: u64 = u64::MAX;
            let mut io_hi: u64 = 0;

            for child in &self.devices {
                if child.addr.bus() < sec || child.addr.bus() > sub {
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

                    if bar.prefetchable {
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

            // Enable memory + IO + bus master on the bridge.
            let new_cmd =
                self.devices[i].original_command | PCI_CMD_IO | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER;
            self.write16(baddr, PCI_COMMAND, new_cmd);
        }
        Ok(())
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl PciEcam {
    /// Enumerate the PCI hierarchy, allocate BAR resources, and enable decode.
    pub fn enumerate_and_allocate(&mut self) -> Result<(), PciEcamError> {
        self.enumerate_bus(self.bus_start)?;

        if self.fixed_bars.iter().any(|fixed| {
            !self.devices.iter().any(|dev| {
                dev.addr == fixed.address
                    && dev
                        .bars
                        .iter()
                        .any(|bar| bar.fixed && bar.reg == fixed.register)
            })
        }) {
            return Err(PciEcamError::FixedBarInvalid);
        }

        if !self.devices.is_empty() {
            self.allocate_resources()?;
        }

        Ok(())
    }

    /// Construct an ECAM root bridge from a runtime provider.
    ///
    /// The provider is queried once, after memory discovery, and the returned
    /// allocation windows are copied into the allocator as a stable snapshot.
    pub fn from_provider<P>(provider: &P) -> Result<Self, PciEcamError>
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
            devices: HVec::new(),
            fixed_bars: PciFixedBars::new(),
            next_bus: info.bus_start.saturating_add(1),
        })
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

        Ok(Self {
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
            devices: HVec::new(),
            fixed_bars: PciFixedBars::new(),
            next_bus: config.bus_start + 1,
        })
    }
}

impl PciEcam {
    pub fn config_read32(&self, addr: PciAddress, reg: u16) -> u32 {
        self.read32(addr, reg)
    }

    pub fn config_write32(&self, addr: PciAddress, reg: u16, val: u32) {
        self.write32(addr, reg, val);
    }

    pub fn ecam_base(&self) -> u64 {
        self.ecam_base as u64
    }

    pub fn ecam_size(&self) -> u64 {
        self.ecam_size as u64
    }

    pub fn bus_start(&self) -> u8 {
        self.bus_start
    }

    pub fn bus_end(&self) -> u8 {
        self.bus_end
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    pub fn windows(&self) -> &[PciWindow] {
        &self.windows[..self.window_count]
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
        let mut config_space = vec![0u32; 1 << 18];
        let ecam_base = config_space.as_mut_ptr() as usize;
        let dummy = PciWindow {
            kind: PciWindowKind::Io,
            base: 0,
            size: 0,
            prefetchable: false,
        };
        (
            PciEcam {
                segment: 0,
                ecam_base,
                ecam_size: 1 << 20,
                bus_start: 0,
                bus_end: 0,
                mmio32: ResourcePool::new(0x8000_0000, 0x1000_0000).unwrap(),
                mmio64: ResourcePool::empty(),
                io_pool: ResourcePool::new(0x1000, 0xf000).unwrap(),
                windows: [dummy; MAX_WINDOWS],
                window_count: 0,
                devices: HVec::new(),
                fixed_bars: PciFixedBars::new(),
                next_bus: 1,
            },
            config_space,
        )
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

        assert_eq!(pci.segment, 3);
        assert_eq!(pci.ecam_base(), 0xe000_0000);
        assert_eq!(pci.ecam_size(), 64 * 1024 * 1024);
        assert_eq!(pci.bus_start(), 0x40);
        assert_eq!(pci.bus_end(), 0x7f);
        assert_eq!(pci.windows().len(), 2);
        assert_eq!(pci.windows()[0].kind, PciWindowKind::Mmio);
        assert_eq!(pci.windows()[1].kind, PciWindowKind::Io);
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

        assert_eq!(pci.io_pool.allocate_aligned(0x20, 0x20), Some(0x1000));
        assert_eq!(pci.io_pool.allocate_aligned(0x20, 0x20), Some(0x1020));
        assert_eq!(pci.io_pool.allocate_aligned(0x20, 0x20), Some(0x1060));
        assert_eq!(pci.windows().len(), 0);
    }

    #[test]
    fn fixed_bar_probe_preserves_bar_and_restores_original_decode_policy() {
        let (mut pci, _config_space) = test_pci();
        let smbus = PciAddress::new(0, 0, 0x1f, 3);
        pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
        pci.write32(smbus, PCI_HEADER_TYPE, 0);
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

        let dev = pci.probe_device(smbus).unwrap().unwrap();
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
        pci.write32(host, PCI_HEADER_TYPE, 0);
        pci.write16(host, PCI_COMMAND, PCI_CMD_MEMORY);
        pci.write32(host, PCI_BAR0, 0xdead_beef);

        let dev = pci.probe_device(host).unwrap().unwrap();

        assert!(dev.bars.iter().all(|bar| bar.bar_type == BarType::None));
        assert_eq!(pci.read32(host, PCI_BAR0), 0xdead_beef);
        assert_eq!(pci.read16(host, PCI_COMMAND), PCI_CMD_MEMORY);
    }

    #[test]
    fn fixed_bar_probe_rejects_live_base_mismatch() {
        let (mut pci, _config_space) = test_pci();
        let smbus = PciAddress::new(0, 0, 0x1f, 3);
        pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
        pci.write32(smbus, PCI_HEADER_TYPE, 0);
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
            pci.probe_device(smbus),
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
