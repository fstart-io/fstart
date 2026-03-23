//! Bochs VBE display driver — MMIO mode for QEMU `bochs-display`.
//!
//! Initializes the bochs/stdvga display via the VBE DISPI register
//! interface.  On non-x86 platforms (AArch64, RISC-V) there are no
//! legacy VGA I/O ports, so this driver uses the MMIO registers exposed
//! via PCI BAR2 (offset 0x500 for DISPI regs, 0x400 for VGA regs).
//!
//! The driver scans the PCI ECAM config space to find the bochs-display
//! device (vendor 0x1234, device 0x1111), reads its allocated BARs, and
//! programs the requested resolution.
//!
//! Compatible: `"bochs-display"`, `"qemu-stdvga"`.
//!
//! # PCI device variants
//!
//! | QEMU device            | Vendor | Device | Class  | FB BAR | MMIO BAR |
//! |------------------------|--------|--------|--------|--------|----------|
//! | `-device bochs-display`| 0x1234 | 0x1111 | 0x0380 | BAR0   | BAR2     |
//! | `-vga std` (stdvga)    | 0x1234 | 0x1111 | 0x0300 | BAR0   | legacy   |
//!
//! This driver targets `bochs-display` (class 0x0380, MMIO via BAR2).

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::framebuffer::{Framebuffer, FramebufferInfo};
use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------
// VBE DISPI register indices
// -----------------------------------------------------------------------

const VBE_DISPI_INDEX_ID: u16 = 0x0;
const VBE_DISPI_INDEX_XRES: u16 = 0x1;
const VBE_DISPI_INDEX_YRES: u16 = 0x2;
const VBE_DISPI_INDEX_BPP: u16 = 0x3;
const VBE_DISPI_INDEX_ENABLE: u16 = 0x4;
const VBE_DISPI_INDEX_BANK: u16 = 0x5;
const VBE_DISPI_INDEX_VIRT_WIDTH: u16 = 0x6;
const VBE_DISPI_INDEX_VIRT_HEIGHT: u16 = 0x7;
const VBE_DISPI_INDEX_X_OFFSET: u16 = 0x8;
const VBE_DISPI_INDEX_Y_OFFSET: u16 = 0x9;

/// VBE version ID mask: `(id & 0xFFF0) == 0xB0C0`.
const VBE_DISPI_ID_MASK: u16 = 0xFFF0;
const VBE_DISPI_ID_MAGIC: u16 = 0xB0C0;

/// Enable flags for `VBE_DISPI_INDEX_ENABLE`.
const VBE_DISPI_ENABLED: u16 = 0x01;
const VBE_DISPI_LFB_ENABLED: u16 = 0x40;

// BAR2 MMIO offsets (bochs-display, non-VGA class 0x0380)
const MMIO_VGA_OFFSET: usize = 0x400;
const MMIO_DISPI_OFFSET: usize = 0x500;

// PCI config space offsets
const PCI_VENDOR_ID: u16 = 0x00;
const PCI_BAR0: u16 = 0x10;
const PCI_BAR2: u16 = 0x18;

/// Bochs display PCI vendor:device.
const BOCHS_VENDOR_ID: u16 = 0x1234;
const BOCHS_DEVICE_ID: u16 = 0x1111;

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the bochs display driver.
///
/// The driver scans PCI config space via ECAM to find the bochs-display
/// device and read its allocated BARs. Resolution and BPP are configurable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BochsDisplayConfig {
    /// ECAM base address — needed to read PCI config space for the
    /// bochs-display device's BARs. Must match the PCI ECAM driver's
    /// `ecam_base` from the board RON.
    pub ecam_base: u64,
    /// Horizontal resolution in pixels.
    pub width: u16,
    /// Vertical resolution in pixels.
    pub height: u16,
}

// -----------------------------------------------------------------------
// Driver struct
// -----------------------------------------------------------------------

/// Bochs VBE display driver.
///
/// After `init()`, the framebuffer is programmed at the requested
/// resolution with 32-bit XRGB8888 pixels. Call `info()` to get the
/// physical address and layout.
pub struct BochsDisplay {
    config: BochsDisplayConfig,
    /// Framebuffer physical address (from PCI BAR0, read during init).
    fb_base: u64,
    /// MMIO register base (from PCI BAR2, read during init).
    mmio_base: u64,
    /// Whether init() has been called successfully.
    initialized: bool,
}

// SAFETY: MMIO registers are hardware-fixed addresses from PCI BARs.
// The driver is used single-threaded during firmware init.
unsafe impl Send for BochsDisplay {}
unsafe impl Sync for BochsDisplay {}

impl BochsDisplay {
    /// Read a 32-bit value from PCI ECAM config space.
    fn ecam_read32(&self, bus: u8, dev: u8, func: u8, reg: u16) -> u32 {
        let offset = ((bus as usize) << 20)
            | ((dev as usize) << 15)
            | ((func as usize) << 12)
            | ((reg as usize) & 0xFFC);
        let addr = self.config.ecam_base as usize + offset;
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read32(addr as *const u32) }
    }

    /// Scan PCI bus 0 for bochs-display (0x1234:0x1111).
    ///
    /// Returns `(bus, dev, func)` of the first match, or `None`.
    fn find_device(&self) -> Option<(u8, u8, u8)> {
        for dev in 0..32u8 {
            let vendor_device = self.ecam_read32(0, dev, 0, PCI_VENDOR_ID);
            if vendor_device == 0xFFFF_FFFF {
                continue;
            }
            let vendor = vendor_device as u16;
            let device = (vendor_device >> 16) as u16;
            if vendor == BOCHS_VENDOR_ID && device == BOCHS_DEVICE_ID {
                return Some((0, dev, 0));
            }
        }
        None
    }

    /// Read a BAR value from PCI config space.
    ///
    /// For 64-bit BARs, reads both the low and high 32-bit halves.
    fn read_bar(&self, bus: u8, dev: u8, func: u8, bar_offset: u16) -> u64 {
        let lo = self.ecam_read32(bus, dev, func, bar_offset);
        if lo & 1 != 0 {
            // I/O BAR — return the I/O base.
            return (lo & 0xFFFF_FFFC) as u64;
        }
        let mem_type = (lo >> 1) & 0x3;
        let base_lo = (lo & 0xFFFF_FFF0) as u64;
        if mem_type == 2 {
            // 64-bit BAR — read upper half.
            let hi = self.ecam_read32(bus, dev, func, bar_offset + 4);
            base_lo | ((hi as u64) << 32)
        } else {
            base_lo
        }
    }

    /// Write a 16-bit VBE DISPI register via MMIO.
    fn dispi_write(&self, index: u16, val: u16) {
        let addr = self.mmio_base as usize + MMIO_DISPI_OFFSET + (index as usize) * 2;
        // SAFETY: BAR2 MMIO region is mapped and valid after PCI init.
        unsafe { fstart_mmio::write16(addr as *mut u16, val) };
    }

    /// Read a 16-bit VBE DISPI register via MMIO.
    fn dispi_read(&self, index: u16) -> u16 {
        let addr = self.mmio_base as usize + MMIO_DISPI_OFFSET + (index as usize) * 2;
        // SAFETY: BAR2 MMIO region is mapped and valid after PCI init.
        unsafe { fstart_mmio::read16(addr as *const u16) }
    }

    /// Write an 8-bit VGA register via MMIO.
    fn vga_write(&self, index: usize, val: u8) {
        let addr = self.mmio_base as usize + MMIO_VGA_OFFSET + index;
        // SAFETY: BAR2 MMIO region is mapped and valid after PCI init.
        unsafe { fstart_mmio::write8(addr as *mut u8, val) };
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl Device for BochsDisplay {
    const NAME: &'static str = "bochs-display";
    const COMPATIBLE: &'static [&'static str] = &["bochs-display", "qemu-stdvga"];
    type Config = BochsDisplayConfig;

    fn new(config: &BochsDisplayConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            config: *config,
            fb_base: 0,
            mmio_base: 0,
            initialized: false,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Step 1: Find the bochs-display PCI device.
        let (bus, dev, func) = self.find_device().ok_or_else(|| {
            fstart_log::error!("bochs-display: PCI device 1234:1111 not found");
            DeviceError::InitFailed
        })?;

        fstart_log::info!(
            "bochs-display: found at PCI {:02x}:{:02x}.{}",
            bus,
            dev,
            func,
        );

        // Step 2: Read BARs (allocated by PCI ECAM driver during PciInit).
        self.fb_base = self.read_bar(bus, dev, func, PCI_BAR0);
        self.mmio_base = self.read_bar(bus, dev, func, PCI_BAR2);

        if self.fb_base == 0 || self.mmio_base == 0 {
            fstart_log::error!("bochs-display: BAR0 or BAR2 not allocated");
            return Err(DeviceError::InitFailed);
        }

        fstart_log::info!("bochs-display: FB BAR0 = {}", fstart_log::Hex(self.fb_base));
        fstart_log::info!(
            "bochs-display: MMIO BAR2 = {}",
            fstart_log::Hex(self.mmio_base),
        );

        // Step 3: Detect the VBE DISPI interface.
        let id = self.dispi_read(VBE_DISPI_INDEX_ID);
        if (id & VBE_DISPI_ID_MASK) != VBE_DISPI_ID_MAGIC {
            fstart_log::error!("bochs-display: VBE DISPI ID mismatch: {}", id);
            return Err(DeviceError::InitFailed);
        }
        fstart_log::info!("bochs-display: VBE DISPI version {}", id);

        // Step 4: Program the display mode.
        // Exact sequence from coreboot's bochs_init_linear_fb():
        let w = self.config.width;
        let h = self.config.height;

        self.dispi_write(VBE_DISPI_INDEX_ENABLE, 0); // disable first
        self.dispi_write(VBE_DISPI_INDEX_BANK, 0);
        self.dispi_write(VBE_DISPI_INDEX_BPP, 32); // 32bpp XRGB8888
        self.dispi_write(VBE_DISPI_INDEX_XRES, w);
        self.dispi_write(VBE_DISPI_INDEX_YRES, h);
        self.dispi_write(VBE_DISPI_INDEX_VIRT_WIDTH, w);
        self.dispi_write(VBE_DISPI_INDEX_VIRT_HEIGHT, h);
        self.dispi_write(VBE_DISPI_INDEX_X_OFFSET, 0);
        self.dispi_write(VBE_DISPI_INDEX_Y_OFFSET, 0);
        self.dispi_write(
            VBE_DISPI_INDEX_ENABLE,
            VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED,
        );

        // Step 5: Disable VGA blanking via MMIO VGA attribute register.
        self.vga_write(0, 0x20);

        self.initialized = true;

        fstart_log::info!(
            "bochs-display: {}x{} @ 32bpp, stride={} bytes",
            w,
            h,
            (w as u32) * 4,
        );

        Ok(())
    }
}

// -----------------------------------------------------------------------
// Framebuffer service trait
// -----------------------------------------------------------------------

impl Framebuffer for BochsDisplay {
    fn info(&self) -> FramebufferInfo {
        FramebufferInfo {
            base_addr: self.fb_base,
            width: self.config.width as u32,
            height: self.config.height as u32,
            stride: self.config.width as u32, // pixels per scanline
            bits_per_pixel: 32,
            // XRGB8888 (bochs VBE native format):
            // byte order [B, G, R, X] in memory = blue at bit 0.
            red_pos: 16,
            red_size: 8,
            green_pos: 8,
            green_size: 8,
            blue_pos: 0,
            blue_size: 8,
        }
    }
}
