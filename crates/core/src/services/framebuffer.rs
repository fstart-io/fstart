//! Framebuffer service — the common typed linear-framebuffer handoff.
//!
//! Display init is platform-owned (QEMU bochs init, future Intel/AMD init):
//! each programs its own hardware, then advertises the resulting mode here.
//! This mirrors coreboot's `struct lb_framebuffer` published via
//! `fb_add_framebuffer_info()`. Payloads (UEFI GOP, Linux efifb) consume
//! this handoff without knowing which init programmed the mode.
//!
//! A device implementing `Framebuffer` provides a linear framebuffer that
//! can be passed to a UEFI GOP implementation or used directly for early
//! display output. The framebuffer is typically backed by PCI BAR memory
//! (e.g., bochs-display, virtio-gpu) or a SoC display controller.

/// Information about a configured linear framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical base address of the framebuffer memory.
    pub base_addr: u64,
    /// Horizontal resolution in pixels.
    pub width: u32,
    /// Vertical resolution in pixels.
    pub height: u32,
    /// Pixels per scanline (may be wider than `width` for alignment).
    pub stride: u32,
    /// Bits per pixel (typically 32).
    pub bits_per_pixel: u8,
    /// Bit position of the red channel.
    pub red_pos: u8,
    /// Number of bits in the red channel.
    pub red_size: u8,
    /// Bit position of the green channel.
    pub green_pos: u8,
    /// Number of bits in the green channel.
    pub green_size: u8,
    /// Bit position of the blue channel.
    pub blue_pos: u8,
    /// Number of bits in the blue channel.
    pub blue_size: u8,
}

/// A device that provides a linear framebuffer for display output.
///
/// After the display init step, the framebuffer is programmed and ready to
/// use. Call `info()` to get the physical address, resolution, and pixel
/// format.
pub trait Framebuffer: Send + Sync {
    /// Return information about the configured framebuffer.
    fn info(&self) -> FramebufferInfo;
}
