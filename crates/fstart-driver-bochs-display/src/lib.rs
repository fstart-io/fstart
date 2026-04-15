//! Bochs VBE display driver — PCI bus child, MMIO mode.
//!
//! A PCI bus child device that uses the parent [`PciRootBus`] to read
//! BARs at its known device:function address.  After construction the
//! driver programs the VBE DISPI registers via BAR2 MMIO.
//!
//! On non-x86 platforms (AArch64, RISC-V) there are no legacy VGA I/O
//! ports, so this driver uses the MMIO registers exposed via PCI BAR2
//! (offset 0x500 for DISPI regs, 0x400 for VGA regs).
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

use fstart_services::device::{BusDevice, DeviceError};
use fstart_services::framebuffer::{Framebuffer, FramebufferInfo};
use fstart_services::pci::{
    PciAddr, PciRootBus, PCI_BAR0, PCI_BAR2, PCI_VENDOR_ID, PCI_VENDOR_INVALID,
};
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

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the bochs display driver.
///
/// As a PCI bus child, the driver receives its ECAM base from the parent
/// [`PciRootBus`] and only needs the device:function address on the bus.
/// BARs are read via the parent's config-space accessors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BochsDisplayConfig {
    /// PCI device number on the bus (bus number comes from the parent).
    pub device: u8,
    /// PCI function number.
    pub function: u8,
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
/// Constructed via [`BusDevice::new_on_bus`] with a parent [`PciRootBus`].
/// BARs are read from PCI config space during construction.  After
/// `init()`, the framebuffer is programmed at the requested resolution
/// with 32-bit XRGB8888 pixels.  Call `info()` to get the physical
/// address and layout.
pub struct BochsDisplay {
    config: BochsDisplayConfig,
    /// Framebuffer physical address (from PCI BAR0).
    fb_base: u64,
    /// MMIO register base (from PCI BAR2).
    mmio_base: u64,
    /// Whether init() has been called successfully.
    initialized: bool,
}

// SAFETY: MMIO registers are hardware-fixed addresses from PCI BARs.
// The driver is used single-threaded during firmware init.
unsafe impl Send for BochsDisplay {}
unsafe impl Sync for BochsDisplay {}

impl BochsDisplay {
    /// Read a BAR value from PCI config space via the parent bus.
    ///
    /// For 64-bit BARs, reads both the low and high 32-bit halves.
    fn read_bar(bus: &dyn PciRootBus, addr: PciAddr, bar_offset: u16) -> u64 {
        let lo = bus.config_read32(addr, bar_offset).unwrap_or(0);
        if lo & 1 != 0 {
            // I/O BAR
            return (lo & 0xFFFF_FFFC) as u64;
        }
        let mem_type = (lo >> 1) & 0x3;
        let base_lo = (lo & 0xFFFF_FFF0) as u64;
        if mem_type == 2 {
            // 64-bit BAR
            let hi = bus.config_read32(addr, bar_offset + 4).unwrap_or(0);
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
// BusDevice trait — PCI bus child
// -----------------------------------------------------------------------

impl BusDevice for BochsDisplay {
    const NAME: &'static str = "bochs-display";
    const COMPATIBLE: &'static [&'static str] = &["bochs-display", "qemu-stdvga"];
    type Config = BochsDisplayConfig;
    type Bus = dyn PciRootBus;

    fn new_on_bus(config: &BochsDisplayConfig, bus: &dyn PciRootBus) -> Result<Self, DeviceError> {
        let addr = PciAddr {
            bus: bus.bus_start(),
            dev: config.device,
            func: config.function,
        };

        // Verify the device is present by reading vendor:device ID.
        let vendor_device = bus
            .config_read32(addr, PCI_VENDOR_ID)
            .map_err(|_| DeviceError::BusError)?;
        if vendor_device == PCI_VENDOR_INVALID {
            fstart_log::error!(
                "bochs-display: no PCI device at {:02x}:{:02x}.{}",
                addr.bus,
                addr.dev,
                addr.func
            );
            return Err(DeviceError::InitFailed);
        }

        let vendor = vendor_device as u16;
        let device = (vendor_device >> 16) as u16;
        if vendor != 0x1234 || device != 0x1111 {
            fstart_log::error!(
                "bochs-display: expected 1234:1111, found {:04x}:{:04x}",
                vendor,
                device
            );
            return Err(DeviceError::InitFailed);
        }

        // Read BARs (allocated by PCI ECAM driver during PciInit).
        let fb_base = Self::read_bar(bus, addr, PCI_BAR0);
        let mmio_base = Self::read_bar(bus, addr, PCI_BAR2);

        fstart_log::info!(
            "bochs-display: PCI {:02x}:{:02x}.{}, FB={}  MMIO={}",
            addr.bus,
            addr.dev,
            addr.func,
            fstart_log::Hex(fb_base),
            fstart_log::Hex(mmio_base),
        );

        Ok(Self {
            config: *config,
            fb_base,
            mmio_base,
            initialized: false,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        if self.fb_base == 0 || self.mmio_base == 0 {
            fstart_log::error!("bochs-display: BAR0 or BAR2 not allocated");
            return Err(DeviceError::InitFailed);
        }

        // Detect the VBE DISPI interface.
        let id = self.dispi_read(VBE_DISPI_INDEX_ID);
        if (id & VBE_DISPI_ID_MASK) != VBE_DISPI_ID_MAGIC {
            fstart_log::error!("bochs-display: VBE DISPI ID mismatch: {}", id);
            return Err(DeviceError::InitFailed);
        }
        fstart_log::info!("bochs-display: VBE DISPI version {}", id);

        // Program the display mode.
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

        // Disable VGA blanking via MMIO VGA attribute register.
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
