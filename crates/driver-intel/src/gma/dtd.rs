//! EDID/VBT detailed timing descriptor decoding.
//!
//! Both the EDID base block and the VBT LFP DVO timing table embed 18-byte
//! detailed timing descriptors, so the on-disk layout and its decoder live here
//! once and are shared.

use tock_registers::register_bitfields;
use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::gma::error::GmaError;
use crate::gma::mode::{Mode, ModeFlags};

register_bitfields![u8,
    /// Upper nibbles of active (7:4) and blanking (3:0) pixel/line counts (offset 0x04 / 0x07, 8 bits).
    ActiveBlankHi [
        BLANK_HI OFFSET(0) NUMBITS(4) [],
        ACTIVE_HI OFFSET(4) NUMBITS(4) []
    ],
    /// Vertical sync offset (7:4) and width (3:0) low nibbles (offset 0x0a, 8 bits).
    VsyncOffWidth [
        VSYNC_WIDTH_LO OFFSET(0) NUMBITS(4) [],
        VSYNC_OFF_LO OFFSET(4) NUMBITS(4) []
    ],
    /// High 2-bit extensions of H/V sync offsets and widths (offset 0x0b, 8 bits).
    SyncHi [
        VSYNC_WIDTH_HI OFFSET(0) NUMBITS(2) [],
        VSYNC_OFF_HI OFFSET(2) NUMBITS(2) [],
        HSYNC_WIDTH_HI OFFSET(4) NUMBITS(2) [],
        HSYNC_OFF_HI OFFSET(6) NUMBITS(2) []
    ],
    /// EDID detailed-timing feature flags (offset 0x11, 8 bits).
    DtdFlags [
        HSYNC_POSITIVE OFFSET(1) NUMBITS(1) [],
        VSYNC_POSITIVE OFFSET(2) NUMBITS(1) [],
        INTERLACE OFFSET(7) NUMBITS(1) []
    ]
];

/// Length of a detailed timing descriptor.
pub const DTD_LEN: usize = 18;

/// One 18-byte detailed timing descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
pub(crate) struct DtdRaw {
    pixel_clock: U16,
    hactive_lo: u8,
    hblank_lo: u8,
    hactive_hblank_hi: u8,
    vactive_lo: u8,
    vblank_lo: u8,
    vactive_vblank_hi: u8,
    hsync_off_lo: u8,
    hsync_width_lo: u8,
    vsync_off_width: u8,
    sync_hi: u8,
    _reserved: [u8; 5],
    flags: u8,
}

impl DtdRaw {
    /// Parse the descriptor if the slice is long enough.
    pub(crate) fn parse(dtd: &[u8]) -> Option<&DtdRaw> {
        DtdRaw::ref_from_prefix(dtd).ok().map(|(raw, _)| raw)
    }

    /// Raw pixel clock in 10 kHz units.
    pub(crate) const fn pixel_clock_raw(&self) -> u16 {
        self.pixel_clock.get()
    }

    /// True when the descriptor is a detailed timing rather than a monitor
    /// descriptor (all timing fields non-zero).
    pub(crate) fn is_timing(&self) -> bool {
        self.pixel_clock_raw() != 0
            && (self.hactive_lo != 0 || ActiveBlankHi::ACTIVE_HI.read(self.hactive_hblank_hi) != 0)
            && (self.hsync_off_lo != 0 || SyncHi::HSYNC_OFF_HI.read(self.sync_hi) != 0)
            && (self.hsync_width_lo != 0 || SyncHi::HSYNC_WIDTH_HI.read(self.sync_hi) != 0)
            && (self.hblank_lo != 0 || ActiveBlankHi::BLANK_HI.read(self.hactive_hblank_hi) != 0)
            && (self.vactive_lo != 0 || ActiveBlankHi::ACTIVE_HI.read(self.vactive_vblank_hi) != 0)
            && (VsyncOffWidth::VSYNC_OFF_LO.read(self.vsync_off_width) != 0
                || SyncHi::VSYNC_OFF_HI.read(self.sync_hi) != 0)
            && (VsyncOffWidth::VSYNC_WIDTH_LO.read(self.vsync_off_width) != 0
                || SyncHi::VSYNC_WIDTH_HI.read(self.sync_hi) != 0)
            && (self.vblank_lo != 0 || ActiveBlankHi::BLANK_HI.read(self.vactive_vblank_hi) != 0)
    }
}

/// True when `dtd` is a detailed timing descriptor (not a monitor descriptor).
pub(crate) fn dtd_present(dtd: &[u8]) -> bool {
    DtdRaw::parse(dtd).map(DtdRaw::is_timing).unwrap_or(false)
}

/// Convert an 18-byte detailed timing descriptor to a mode.
pub fn mode_from_dtd(dtd: &[u8]) -> Result<Mode, GmaError> {
    let raw = DtdRaw::parse(dtd).ok_or(GmaError::ModeUnavailable)?;
    let pixel_clock_khz = u32::from(raw.pixel_clock_raw()) * 10;
    let hactive = u16::from(raw.hactive_lo)
        | (u16::from(ActiveBlankHi::ACTIVE_HI.read(raw.hactive_hblank_hi)) << 8);
    let hblank = u16::from(raw.hblank_lo)
        | (u16::from(ActiveBlankHi::BLANK_HI.read(raw.hactive_hblank_hi)) << 8);
    let vactive = u16::from(raw.vactive_lo)
        | (u16::from(ActiveBlankHi::ACTIVE_HI.read(raw.vactive_vblank_hi)) << 8);
    let vblank = u16::from(raw.vblank_lo)
        | (u16::from(ActiveBlankHi::BLANK_HI.read(raw.vactive_vblank_hi)) << 8);
    let hsync_off =
        u16::from(raw.hsync_off_lo) | (u16::from(SyncHi::HSYNC_OFF_HI.read(raw.sync_hi)) << 8);
    let hsync_width =
        u16::from(raw.hsync_width_lo) | (u16::from(SyncHi::HSYNC_WIDTH_HI.read(raw.sync_hi)) << 8);
    let vsync_off = u16::from(VsyncOffWidth::VSYNC_OFF_LO.read(raw.vsync_off_width))
        | (u16::from(SyncHi::VSYNC_OFF_HI.read(raw.sync_hi)) << 4);
    let vsync_width = u16::from(VsyncOffWidth::VSYNC_WIDTH_LO.read(raw.vsync_off_width))
        | (u16::from(SyncHi::VSYNC_WIDTH_HI.read(raw.sync_hi)) << 4);
    // EDID detailed-timing separate-sync polarity bits: bit 1 is horizontal
    // positive, bit 2 is vertical positive (Linux DRM_EDID_PT_HSYNC_POSITIVE /
    // DRM_EDID_PT_VSYNC_POSITIVE). Missing bits mean negative polarity.
    let mut flags = match (
        DtdFlags::HSYNC_POSITIVE.is_set(raw.flags),
        DtdFlags::VSYNC_POSITIVE.is_set(raw.flags),
    ) {
        (true, true) => ModeFlags::PHSYNC | ModeFlags::PVSYNC,
        (false, true) => ModeFlags::NHSYNC | ModeFlags::PVSYNC,
        (true, false) => ModeFlags::PHSYNC | ModeFlags::NVSYNC,
        (false, false) => ModeFlags::NHSYNC | ModeFlags::NVSYNC,
    };
    if DtdFlags::INTERLACE.is_set(raw.flags) {
        flags |= ModeFlags::INTERLACE;
    }
    let mode = Mode {
        hdisplay: hactive,
        hsync_start: hactive + hsync_off,
        hsync_end: hactive + hsync_off + hsync_width,
        htotal: hactive + hblank,
        vdisplay: vactive,
        vsync_start: vactive + vsync_off,
        vsync_end: vactive + vsync_off + vsync_width,
        vtotal: vactive + vblank,
        pixel_clock_khz,
        flags,
    };
    if mode.is_valid() {
        Ok(mode)
    } else {
        Err(GmaError::ModeUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xga_dtd_with_flags(flags: u8) -> [u8; DTD_LEN] {
        [
            0x64, 0x19, // 65.00 MHz
            0x00, 0x40, 0x41, // hactive 1024, hblank 320
            0x00, 0x26, 0x30, // vactive 768, vblank 38
            0x18, 0x88, 0x36, 0x00, // sync offsets/widths
            0x00, 0x00, 0x00, 0x00, 0x00, flags,
        ]
    }

    #[test]
    fn decodes_xga_timing() {
        let mode = mode_from_dtd(&xga_dtd_with_flags(0x1e)).unwrap();
        assert_eq!(mode.hdisplay, 1024);
        assert_eq!(mode.vdisplay, 768);
        assert_eq!(mode.pixel_clock_khz, 65_000);
        assert!(dtd_present(&xga_dtd_with_flags(0x1e)));
    }

    #[test]
    fn monitor_descriptor_is_not_a_timing() {
        let mut descriptor = [0u8; DTD_LEN];
        descriptor[0..3].copy_from_slice(&[0x00, 0x00, 0x00]);
        descriptor[3] = 0xfc; // monitor name tag
        assert!(!dtd_present(&descriptor));
        assert_eq!(mode_from_dtd(&descriptor), Err(GmaError::ModeUnavailable));
    }

    #[test]
    fn short_descriptor_is_rejected() {
        assert!(!dtd_present(&[0u8; DTD_LEN - 1]));
        assert_eq!(mode_from_dtd(&[0u8; 4]), Err(GmaError::ModeUnavailable));
    }
}
