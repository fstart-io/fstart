//! QEMU bochs-display init — MMIO mode.
//!
//! This is QEMU-platform code, not a generic display driver: it knows the
//! exact QEMU PCI endpoint (VID `0x1234`, DID `0x1111`, class `0x0380`)
//! and programs it after this platform's PCI enumeration has assigned its
//! BARs. Mirrors coreboot's `src/drivers/emulation/qemu/bochs.c`, where the
//! bochs init likewise lives under the QEMU emulation driver and advertises
//! the mode, rather than implementing a reusable display driver.
//!
//! After [`BochsDisplay::init`] the framebuffer is a 32-bit XRGB8888 linear
//! buffer. The mode is handed off as [`FramebufferInfo`] — the common typed
//! linear-framebuffer handoff (coreboot's `struct lb_framebuffer` via
//! `fb_add_framebuffer_info`). Future Intel/AMD display init will produce
//! the same handoff from their own platform flows; payloads (UEFI GOP,
//! Linux efifb) consume it without knowing which init programmed the mode.
//!
//! Only the MMIO register path is implemented (DISPI regs at BAR2 offset
//! `0x500`, VGA regs at `0x400`). `-vga std` (class `0x0300`, no MMIO BAR)
//! is not supported — use `-device bochs-display`.

use fstart_core::mmio::{read16, write8, write16};
use fstart_core::services::device::DeviceError;
use fstart_core::services::framebuffer::{Framebuffer, FramebufferInfo};
use fstart_pci::{PCI_BAR0, PCI_BAR2, PCI_VENDOR_ID, PCI_VENDOR_INVALID, PciAddress, PciEcam};
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

/// Expected PCI identity of `bochs-display`.
const BOCHS_VID: u16 = 0x1234;
const BOCHS_DID: u16 = 0x1111;

// BAR2 MMIO offsets (bochs-display, non-VGA class 0x0380).
const MMIO_VGA_OFFSET: usize = 0x400;
const MMIO_DISPI_OFFSET: usize = 0x500;

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the Bochs display driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BochsDisplayConfig {
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
/// Probe with [`BochsDisplay::probe`] after PCI enumeration, then
/// [`BochsDisplay::init`] to program the mode. Implements
/// [`Framebuffer`] so the mode lands in UEFI GOP unchanged.
pub struct BochsDisplay {
    config: BochsDisplayConfig,
    /// Framebuffer physical address (from PCI BAR0).
    fb_base: u64,
    /// MMIO register base (from PCI BAR2).
    mmio_base: u64,
}

// SAFETY: MMIO registers are hardware-fixed addresses from PCI BARs.
// The driver is used single-threaded during firmware init.
unsafe impl Send for BochsDisplay {}
unsafe impl Sync for BochsDisplay {}

impl BochsDisplay {
    /// Scan every bus owned by `ecam` for `1234:1111` and construct
    /// the driver from its programmed BARs.
    ///
    /// Returns `None` when no bochs-display device is present.
    /// Returns `Err` when the device is present but unusable
    /// (BARs not allocated).
    pub fn probe(ecam: &PciEcam, config: BochsDisplayConfig) -> Result<Option<Self>, DeviceError> {
        let found = Self::find_device(ecam);
        let Some(addr) = found else {
            return Ok(None);
        };
        let fb_base = Self::read_bar(ecam, addr, PCI_BAR0);
        let mmio_base = Self::read_bar(ecam, addr, PCI_BAR2);
        if fb_base == 0 || mmio_base == 0 {
            fstart_log::error!("bochs-display: BAR0 or BAR2 not allocated");
            return Err(DeviceError::InitFailed);
        }
        // Register access casts `mmio_base` to `usize`: reject an
        // unaddressable BAR instead of truncating it silently. Only
        // reachable on 32-bit targets with BAR2 allocated above 4 GiB.
        if mmio_base > usize::MAX as u64 {
            fstart_log::error!("bochs-display: BAR2 above addressable range");
            return Err(DeviceError::InitFailed);
        }
        fstart_log::info!(
            "bochs-display: PCI {:02x}:{:02x}.{}, FB={} MMIO={}",
            addr.bus(),
            addr.device(),
            addr.function(),
            fstart_log::Hex(fb_base),
            fstart_log::Hex(mmio_base),
        );
        Ok(Some(Self {
            config,
            fb_base,
            mmio_base,
        }))
    }

    /// Program the requested mode (32-bit XRGB8888 linear framebuffer).
    pub fn init(&mut self) -> Result<(), DeviceError> {
        // Detect the VBE DISPI interface.
        // SAFETY: BAR2 was allocated from the Q35 MMIO32 window, which the
        // stage identity-maps; valid after PCI init.
        let id = unsafe { self.dispi_read(VBE_DISPI_INDEX_ID) };
        if (id & VBE_DISPI_ID_MASK) != VBE_DISPI_ID_MAGIC {
            fstart_log::error!("bochs-display: VBE DISPI ID mismatch: {:#06x}", id);
            return Err(DeviceError::InitFailed);
        }
        fstart_log::info!("bochs-display: VBE DISPI version {:#06x}", id);

        // Program the display mode.
        // Exact sequence from coreboot's bochs_init_linear_fb().
        let w = self.config.width;
        let h = self.config.height;
        // SAFETY: BAR2 was allocated from the Q35 MMIO32 window, which the
        // stage identity-maps; valid after PCI init.
        unsafe {
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
        }

        fstart_log::info!(
            "bochs-display: {}x{} @ 32bpp, stride={} bytes",
            w,
            h,
            (w as u32) * 4,
        );
        Ok(())
    }

    /// Convenience: probe, init, and return the framebuffer description.
    /// Returns `Ok(None)` when no device is present.
    pub fn probe_and_init(
        ecam: &PciEcam,
        config: BochsDisplayConfig,
    ) -> Result<Option<FramebufferInfo>, DeviceError> {
        let Some(mut display) = Self::probe(ecam, config)? else {
            return Ok(None);
        };
        display.init()?;
        Ok(Some(display.info()))
    }

    fn find_device(ecam: &PciEcam) -> Option<PciAddress> {
        for bus in ecam.bus_start()..=ecam.bus_end() {
            for dev in 0..32u8 {
                // Function 0 decides whether the slot exists at all.
                let f0 = PciAddress::new(0, bus, dev, 0);
                if ecam.config_read32(f0, PCI_VENDOR_ID) == PCI_VENDOR_INVALID {
                    continue;
                }
                for func in 0..8u8 {
                    let addr = PciAddress::new(0, bus, dev, func);
                    let id = ecam.config_read32(addr, PCI_VENDOR_ID);
                    if id == PCI_VENDOR_INVALID {
                        continue;
                    }
                    if id as u16 == BOCHS_VID && (id >> 16) as u16 == BOCHS_DID {
                        return Some(addr);
                    }
                }
            }
        }
        None
    }

    /// Read a BAR value from PCI config space (handles 64-bit BARs).
    fn read_bar(ecam: &PciEcam, addr: PciAddress, bar_offset: u16) -> u64 {
        let lo = ecam.config_read32(addr, bar_offset);
        if lo & 1 != 0 {
            // I/O BAR — not usable as a framebuffer.
            return 0;
        }
        let mem_type = (lo >> 1) & 0x3;
        let base_lo = (lo & 0xFFFF_FFF0) as u64;
        if mem_type == 2 {
            // 64-bit BAR: combine with the high half.
            let hi = ecam.config_read32(addr, bar_offset + 4);
            base_lo | ((hi as u64) << 32)
        } else {
            base_lo
        }
    }

    /// Write a 16-bit VBE DISPI register via MMIO.
    ///
    /// # Safety
    /// `mmio_base` must be a mapped MMIO region: the Q35 stage identity-maps
    /// the enumerated MMIO32 window it was allocated from (`probe` rejects
    /// an unaddressable BAR outright).
    unsafe fn dispi_write(&self, index: u16, val: u16) {
        let addr = self.mmio_base as usize + MMIO_DISPI_OFFSET + (index as usize) * 2;
        unsafe { write16(addr as *mut u16, val) };
    }

    /// Read a 16-bit VBE DISPI register via MMIO.
    ///
    /// # Safety
    /// `mmio_base` must be a mapped MMIO region: the Q35 stage identity-maps
    /// the enumerated MMIO32 window it was allocated from (`probe` rejects
    /// an unaddressable BAR outright).
    unsafe fn dispi_read(&self, index: u16) -> u16 {
        let addr = self.mmio_base as usize + MMIO_DISPI_OFFSET + (index as usize) * 2;
        unsafe { read16(addr as *const u16) }
    }

    /// Write an 8-bit VGA register via MMIO.
    ///
    /// # Safety
    /// `mmio_base` must be a mapped MMIO region: the Q35 stage identity-maps
    /// the enumerated MMIO32 window it was allocated from (`probe` rejects
    /// an unaddressable BAR outright).
    unsafe fn vga_write(&self, index: usize, val: u8) {
        let addr = self.mmio_base as usize + MMIO_VGA_OFFSET + index;
        unsafe { write8(addr as *mut u8, val) };
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
