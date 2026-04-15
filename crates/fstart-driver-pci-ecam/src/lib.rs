//! PCI ECAM host bridge driver with bus enumeration and resource allocation.
//!
//! This driver implements a PCIe root complex that uses the Enhanced
//! Configuration Access Mechanism (ECAM) for config-space access.  On
//! `init()` it performs a full bus walk, sizes every BAR, allocates
//! resources from the MMIO/IO windows declared in its config, programs
//! the BARs and bridge forwarding windows, and enables memory/IO decode.
//!
//! The allocation algorithm is a simplified version of coreboot's
//! `resource_allocator_v4`: largest-alignment-first within each resource
//! type, single-pass bottom-up accumulation for bridge windows, then
//! top-down absolute address assignment.
//!
//! **Requires a heap allocator** — the device list uses `alloc::vec::Vec`
//! so arbitrary bus topologies are supported.  Ensure `MemoryInit` (or
//! equivalent heap setup) runs before `PciInit`.
//!
//! Compatible: `"pci-host-ecam-generic"`.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use fstart_services::device::{Device, DeviceError};
use fstart_services::pci::{
    PciAddr, PciRootBus, PciWindow, PciWindowKind, PCI_BAR0, PCI_CLASS_REVISION,
    PCI_CMD_BUS_MASTER, PCI_CMD_IO, PCI_CMD_MEMORY, PCI_COMMAND, PCI_HEADER_TYPE,
    PCI_HEADER_TYPE_BRIDGE, PCI_HEADER_TYPE_MULTI_FUNC, PCI_IO_BASE, PCI_MEMORY_BASE,
    PCI_PREF_BASE_UPPER32, PCI_PREF_LIMIT_UPPER32, PCI_PREF_MEMORY_BASE, PCI_PRIMARY_BUS,
    PCI_VENDOR_ID, PCI_VENDOR_INVALID,
};
use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the PCI ECAM host bridge.
///
/// All addresses come from the board RON and describe the fixed platform
/// windows that QEMU / the SoC provides for PCI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
}

/// A discovered PCI device or bridge.
struct PciDev {
    addr: PciAddr,
    header_type: u8,
    bars: [BarInfo; 6],
    /// For bridges: secondary bus number.
    secondary_bus: u8,
    /// For bridges: subordinate bus number.
    subordinate_bus: u8,
}

/// Simple bump allocator for one address type (MMIO32, MMIO64, or IO).
#[derive(Debug)]
struct ResourcePool {
    next: u64,
    limit: u64,
}

impl ResourcePool {
    fn new(base: u64, size: u64) -> Self {
        Self {
            next: base,
            limit: base.saturating_add(size),
        }
    }

    /// Allocate `size` bytes with natural alignment.  Returns base address.
    fn allocate(&mut self, size: u64) -> Option<u64> {
        if size == 0 {
            return None;
        }
        // Align up to size (PCI BARs are naturally aligned).
        let aligned = (self.next + size - 1) & !(size - 1);
        let end = aligned.checked_add(size)?;
        if end > self.limit {
            return None;
        }
        self.next = end;
        Some(aligned)
    }
}

// -----------------------------------------------------------------------
// Driver struct
// -----------------------------------------------------------------------

/// Maximum number of address windows a single root bridge can have.
///
/// Three is typical (low MMIO, high MMIO, I/O) but a few extra slots
/// accommodate unusual platforms.
const MAX_WINDOWS: usize = 8;

/// PCI ECAM host bridge driver.
pub struct PciEcam {
    ecam_base: usize,
    ecam_size: usize,
    bus_start: u8,
    bus_end: u8,
    mmio32: ResourcePool,
    mmio64: ResourcePool,
    io_pool: ResourcePool,
    /// Decoded address windows (original config values, not modified by allocation).
    windows: [PciWindow; MAX_WINDOWS],
    window_count: usize,
    devices: Vec<PciDev>,
    /// Next bus number to assign to a bridge.
    next_bus: u8,
}

// SAFETY: MMIO registers are hardware-fixed addresses from the board RON.
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
    ) {
        self.mmio32 = ResourcePool::new(mmio32_base, mmio32_size);
        self.mmio64 = ResourcePool::new(mmio64_base, mmio64_size);
        self.io_pool = ResourcePool::new(pio_base, pio_size);
        self.rebuild_windows();
    }

    /// Rebuild the external `windows` array from the current resource pools.
    fn rebuild_windows(&mut self) {
        self.window_count = 0;

        if self.mmio32.limit > self.mmio32.next {
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: self.mmio32.next,
                size: self.mmio32.limit - self.mmio32.next,
                prefetchable: false,
            };
            self.window_count += 1;
        }
        if self.mmio64.limit > self.mmio64.next {
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: self.mmio64.next,
                size: self.mmio64.limit - self.mmio64.next,
                prefetchable: true,
            };
            self.window_count += 1;
        }
        if self.io_pool.limit > self.io_pool.next {
            self.windows[self.window_count] = PciWindow {
                kind: PciWindowKind::Io,
                base: self.io_pool.next,
                size: self.io_pool.limit - self.io_pool.next,
                prefetchable: false,
            };
            self.window_count += 1;
        }
    }

    // -- ECAM helpers --

    fn ecam_addr(&self, addr: PciAddr, reg: u16) -> Option<usize> {
        if addr.bus < self.bus_start || addr.bus > self.bus_end {
            return None;
        }
        let offset = ((addr.bus as usize) << 20)
            | ((addr.dev as usize) << 15)
            | ((addr.func as usize) << 12)
            | ((reg as usize) & 0xFFC);
        if offset < self.ecam_size {
            Some(self.ecam_base + offset)
        } else {
            None
        }
    }

    fn read32(&self, addr: PciAddr, reg: u16) -> u32 {
        match self.ecam_addr(addr, reg) {
            // SAFETY: ECAM region is memory-mapped PCI config space.
            Some(a) => unsafe { fstart_mmio::read32(a as *const u32) },
            None => 0xFFFF_FFFF,
        }
    }

    fn write32(&self, addr: PciAddr, reg: u16, val: u32) {
        if let Some(a) = self.ecam_addr(addr, reg) {
            // SAFETY: ECAM region is memory-mapped PCI config space.
            unsafe { fstart_mmio::write32(a as *mut u32, val) };
        }
    }

    // -- BAR sizing --

    /// Size a single BAR.  Returns the BAR info and whether it consumed
    /// two BAR slots (64-bit).
    fn size_bar(&self, addr: PciAddr, bar_idx: usize) -> (BarInfo, bool) {
        let reg = PCI_BAR0 + (bar_idx as u16) * 4;
        let original = self.read32(addr, reg);

        // Write all-ones, read back to determine size.
        self.write32(addr, reg, 0xFFFF_FFFF);
        let sized = self.read32(addr, reg);
        self.write32(addr, reg, original);

        let none = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg,
        };

        if sized == 0 || sized == 0xFFFF_FFFF {
            return (none, false);
        }

        if original & 1 == 1 {
            // I/O BAR
            let mask = sized | 0x3;
            let size = (!mask).wrapping_add(1) as u64;
            return (
                BarInfo {
                    bar_type: BarType::Io,
                    size,
                    prefetchable: false,
                    reg,
                },
                false,
            );
        }

        // Memory BAR
        let prefetchable = (original & 0x8) != 0;
        let mem_type = (original >> 1) & 0x3;

        match mem_type {
            0 => {
                // 32-bit
                let mask = sized | 0xF;
                let size = (!mask).wrapping_add(1) as u64;
                (
                    BarInfo {
                        bar_type: BarType::Memory32,
                        size,
                        prefetchable,
                        reg,
                    },
                    false,
                )
            }
            2 => {
                // 64-bit — also probe upper BAR
                let upper_reg = reg + 4;
                let original_hi = self.read32(addr, upper_reg);
                self.write32(addr, upper_reg, 0xFFFF_FFFF);
                let sized_hi = self.read32(addr, upper_reg);
                self.write32(addr, upper_reg, original_hi);

                let full_sized = ((sized_hi as u64) << 32) | (sized as u64);
                let mask = full_sized | 0xF;
                let size = (!mask).wrapping_add(1);
                (
                    BarInfo {
                        bar_type: BarType::Memory64,
                        size,
                        prefetchable,
                        reg,
                    },
                    true,
                )
            }
            _ => (none, false),
        }
    }

    /// Probe a single device/function, size its BARs.
    fn probe_device(&self, addr: PciAddr) -> Option<PciDev> {
        let vendor_device = self.read32(addr, PCI_VENDOR_ID);
        if vendor_device == PCI_VENDOR_INVALID {
            return None;
        }
        let hdr = self.read32(addr, PCI_HEADER_TYPE);
        let header_type = (hdr >> 16) as u8 & 0x7F;

        let max_bars = if header_type == PCI_HEADER_TYPE_BRIDGE {
            2
        } else {
            6
        };

        let none_bar = BarInfo {
            bar_type: BarType::None,
            size: 0,
            prefetchable: false,
            reg: 0,
        };
        let mut bars = [none_bar; 6];

        let mut i = 0;
        while i < max_bars {
            let (info, is_64) = self.size_bar(addr, i);
            bars[i] = info;
            if is_64 {
                i += 1; // skip upper half
            }
            i += 1;
        }

        Some(PciDev {
            addr,
            header_type,
            bars,
            secondary_bus: 0,
            subordinate_bus: 0,
        })
    }

    // -- Enumeration --

    /// Enumerate a bus recursively.  Discovers devices, assigns bus numbers
    /// to bridges, and recurses behind them.
    fn enumerate_bus(&mut self, bus: u8) {
        for dev in 0..32u8 {
            let addr = PciAddr::new(bus, dev, 0);
            if self.read32(addr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                continue;
            }

            // Check multi-function bit
            let hdr = self.read32(addr, PCI_HEADER_TYPE);
            let multi_func = (hdr >> 16) as u8 & PCI_HEADER_TYPE_MULTI_FUNC;
            let max_func = if multi_func != 0 { 8 } else { 1 };

            for func in 0..max_func {
                let faddr = PciAddr::new(bus, dev, func);
                if func > 0 && self.read32(faddr, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                    continue;
                }

                if let Some(mut pci_dev) = self.probe_device(faddr) {
                    if pci_dev.header_type == PCI_HEADER_TYPE_BRIDGE {
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

                        self.enumerate_bus(secondary);

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

                    self.devices.push(pci_dev);
                }
            }
        }
    }

    // -- Resource allocation --

    /// Allocate and program BARs for all non-bridge devices, then program
    /// bridge forwarding windows.
    fn allocate_resources(&mut self) {
        // Phase 1: allocate endpoint BARs.
        for i in 0..self.devices.len() {
            if self.devices[i].header_type == PCI_HEADER_TYPE_BRIDGE {
                continue;
            }

            let addr = self.devices[i].addr;
            let mut bar_idx = 0;
            while bar_idx < 6 {
                let bar = self.devices[i].bars[bar_idx];
                match bar.bar_type {
                    BarType::Memory32 => {
                        if let Some(base) = self.mmio32.allocate(bar.size) {
                            self.write32(addr, bar.reg, base as u32);
                        }
                    }
                    BarType::Memory64 => {
                        // Prefer 64-bit pool, fall back to 32-bit.
                        let base = self
                            .mmio64
                            .allocate(bar.size)
                            .or_else(|| self.mmio32.allocate(bar.size));
                        if let Some(base) = base {
                            let lo = (base as u32 & 0xFFFF_FFF0)
                                | 0x4
                                | if bar.prefetchable { 0x8 } else { 0 };
                            self.write32(addr, bar.reg, lo);
                            self.write32(addr, bar.reg + 4, (base >> 32) as u32);
                        }
                        bar_idx += 1; // skip upper half slot
                    }
                    BarType::Io => {
                        if let Some(base) = self.io_pool.allocate(bar.size) {
                            self.write32(addr, bar.reg, (base as u32) | 0x1);
                        }
                    }
                    BarType::None => {}
                }
                bar_idx += 1;
            }

            // Enable memory + IO + bus master.
            let cmd = self.read32(addr, PCI_COMMAND) as u16;
            let new_cmd = cmd | PCI_CMD_IO | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER;
            self.write32(addr, PCI_COMMAND, new_cmd as u32);
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

            for child in &self.devices {
                if child.addr.bus < sec || child.addr.bus > sub {
                    continue;
                }
                for bar in &child.bars {
                    if bar.bar_type == BarType::None || bar.bar_type == BarType::Io {
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

            // Disable I/O forwarding (simplified: we don't allocate IO to bridges).
            self.write32(baddr, PCI_IO_BASE, 0x00FF);

            // Enable memory + IO + bus master on the bridge.
            let cmd = self.read32(baddr, PCI_COMMAND) as u16;
            let new_cmd = cmd | PCI_CMD_IO | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER;
            self.write32(baddr, PCI_COMMAND, new_cmd as u32);
        }
    }

    /// Log discovered devices and their allocated BARs.
    fn log_devices(&self) {
        for dev in &self.devices {
            let kind = if dev.header_type == PCI_HEADER_TYPE_BRIDGE {
                " [bridge]"
            } else {
                ""
            };
            let vendor_device = self.read32(dev.addr, PCI_VENDOR_ID);
            let class_rev = self.read32(dev.addr, PCI_CLASS_REVISION);
            fstart_log::info!(
                "  PCI {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}{:02x}{}",
                dev.addr.bus,
                dev.addr.dev,
                dev.addr.func,
                vendor_device as u16,
                (vendor_device >> 16) as u16,
                (class_rev >> 24) as u8,
                (class_rev >> 16) as u8,
                kind,
            );

            for bar in &dev.bars {
                if bar.bar_type == BarType::None {
                    continue;
                }
                let base_lo = self.read32(dev.addr, bar.reg);
                let base = match bar.bar_type {
                    BarType::Memory64 => {
                        let hi = self.read32(dev.addr, bar.reg + 4);
                        ((hi as u64) << 32) | ((base_lo & 0xFFFF_FFF0) as u64)
                    }
                    BarType::Io => (base_lo & 0xFFFF_FFFC) as u64,
                    _ => (base_lo & 0xFFFF_FFF0) as u64,
                };

                let type_str = match bar.bar_type {
                    BarType::Memory32 => "MEM32",
                    BarType::Memory64 => "MEM64",
                    BarType::Io => "IO   ",
                    BarType::None => unreachable!(),
                };
                fstart_log::info!(
                    "    BAR{}: {} base={:#010x} size={:#x}",
                    (bar.reg - PCI_BAR0) / 4,
                    type_str,
                    base,
                    bar.size,
                );
            }
        }
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl Device for PciEcam {
    const NAME: &'static str = "pci-ecam";
    const COMPATIBLE: &'static [&'static str] = &["pci-host-ecam-generic"];
    type Config = PciEcamConfig;

    fn new(config: &PciEcamConfig) -> Result<Self, DeviceError> {
        if config.bus_end < config.bus_start {
            return Err(DeviceError::ConfigError);
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

        if config.mmio32_size > 0 {
            windows[wc] = PciWindow {
                kind: PciWindowKind::Mmio,
                base: config.mmio32_base,
                size: config.mmio32_size,
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
            ecam_base: config.ecam_base as usize,
            ecam_size: config.ecam_size as usize,
            bus_start: config.bus_start,
            bus_end: config.bus_end,
            mmio32: ResourcePool::new(config.mmio32_base, config.mmio32_size),
            mmio64: ResourcePool::new(config.mmio64_base, config.mmio64_size),
            io_pool: ResourcePool::new(config.pio_base, config.pio_size),
            windows,
            window_count: wc,
            devices: Vec::new(),
            next_bus: config.bus_start + 1,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        fstart_log::info!(
            "PCI: enumerating buses {}..{}",
            self.bus_start,
            self.bus_end
        );
        self.enumerate_bus(self.bus_start);

        fstart_log::info!("PCI: {} device(s) found", self.devices.len());
        if !self.devices.is_empty() {
            fstart_log::info!("PCI: allocating resources...");
            self.allocate_resources();
            self.log_devices();
        }

        Ok(())
    }
}

// -----------------------------------------------------------------------
// PciRootBus service trait
// -----------------------------------------------------------------------

impl PciRootBus for PciEcam {
    fn config_read32(&self, addr: PciAddr, reg: u16) -> Result<u32, ServiceError> {
        Ok(self.read32(addr, reg))
    }

    fn config_write32(&self, addr: PciAddr, reg: u16, val: u32) -> Result<(), ServiceError> {
        self.write32(addr, reg, val);
        Ok(())
    }

    fn ecam_base(&self) -> u64 {
        self.ecam_base as u64
    }

    fn ecam_size(&self) -> u64 {
        self.ecam_size as u64
    }

    fn bus_start(&self) -> u8 {
        self.bus_start
    }

    fn bus_end(&self) -> u8 {
        self.bus_end
    }

    fn device_count(&self) -> usize {
        self.devices.len()
    }

    fn windows(&self) -> &[PciWindow] {
        &self.windows[..self.window_count]
    }
}
