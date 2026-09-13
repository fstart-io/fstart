//! EDID helpers for Intel GMA display initialization.
//!
//! This module mirrors libgfxinit's `HW.GFX.EDID`: base-block header/checksum
//! validation, optional header sanitization, display input compatibility, and
//! detailed/standard/established timing conversion for live DDC callers.

use heapless::Vec;

use crate::error::GmaError;
use crate::mode::{Mode, ModeFlags};
use crate::types::Port;
use crate::dtd::{dtd_present, mode_from_dtd};

/// Size in bytes of an EDID base block.
pub const EDID_BLOCK_LEN: usize = 128;

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const ESTABLISHED_TIMINGS_1: usize = 35;
const ESTABLISHED_TIMINGS_2: usize = 36;
const ESTABLISHED_TIMINGS_MFG: usize = 37;
const STANDARD_TIMINGS: usize = 38;
const STANDARD_TIMING_COUNT: usize = 8;
const DESCRIPTOR_1: usize = 54;
const DESCRIPTOR_LEN: usize = 18;
const DESCRIPTOR_COUNT: usize = 4;
const INPUT: usize = 20;
const INPUT_DIGITAL: u8 = 1 << 7;
const EXTENSION_COUNT: usize = 126;
const MAX_PIXEL_CLOCK_SLACK_KHZ: u32 = 5_001;

/// Maximum number of EDID base-block modes returned by the EDID helpers.
pub const MAX_BASE_MODES: usize = 32;
/// Maximum number of EDID extension blocks read during firmware probing.
pub const MAX_EXTENSION_BLOCKS: usize = 4;
/// Maximum number of modes returned after folding base and extension blocks.
pub const MAX_EDID_MODES: usize = 48;

const CEA_EXTENSION_TAG: u8 = 0x02;
const CEA_DTD_START: usize = 2;
const CEA_DATA_BLOCK_START: usize = 4;

/// EDID monitor range limits descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRange {
    /// Minimum vertical refresh in Hz.
    pub min_vrefresh_hz: u16,
    /// Maximum vertical refresh in Hz.
    pub max_vrefresh_hz: u16,
    /// Minimum horizontal scan frequency in kHz.
    pub min_hfreq_khz: u16,
    /// Maximum horizontal scan frequency in kHz.
    pub max_hfreq_khz: u16,
    /// Maximum pixel clock in MHz, when the descriptor provides one.
    pub max_pixel_clock_mhz: Option<u16>,
}

impl MonitorRange {
    /// Return whether a mode lies within the advertised EDID range limits.
    pub fn contains_mode(&self, mode: Mode) -> bool {
        if !mode.is_valid() {
            return false;
        }
        let vrefresh = mode_refresh_hz(mode);
        let hfreq_khz = horizontal_frequency_khz(mode);
        let clock_ok = self
            .max_pixel_clock_mhz
            .map(|max_mhz| {
                mode.pixel_clock_khz <= u32::from(max_mhz) * 1000 + MAX_PIXEL_CLOCK_SLACK_KHZ
            })
            .unwrap_or(true);
        vrefresh >= self.min_vrefresh_hz
            && vrefresh <= self.max_vrefresh_hz
            && hfreq_khz >= u32::from(self.min_hfreq_khz)
            && hfreq_khz <= u32::from(self.max_hfreq_khz)
            && clock_ok
    }
}

/// Borrowed, validated EDID base block.
#[derive(Debug, Clone, Copy)]
pub struct Edid<'a> {
    raw: &'a [u8; EDID_BLOCK_LEN],
}

impl<'a> Edid<'a> {
    /// Validate and borrow an EDID base block.
    pub fn parse(raw: &'a [u8]) -> Result<Self, GmaError> {
        let raw: &'a [u8; EDID_BLOCK_LEN] =
            raw.try_into().map_err(|_| GmaError::ModeUnavailable)?;
        if header_score(raw) != HEADER.len() || checksum(raw) != 0 {
            return Err(GmaError::ModeUnavailable);
        }
        Ok(Self { raw })
    }

    /// Return the raw EDID base block.
    pub const fn raw(&self) -> &'a [u8; EDID_BLOCK_LEN] {
        self.raw
    }

    /// Return true when the sink reports a digital input.
    pub fn is_digital(&self) -> bool {
        (self.raw[INPUT] & INPUT_DIGITAL) != 0
    }

    /// Return true when the sink reports an analog input.
    pub fn is_analog(&self) -> bool {
        !self.is_digital()
    }

    /// Number of EDID extension blocks advertised by the base block.
    pub fn extension_count(&self) -> u8 {
        self.raw[EXTENSION_COUNT]
    }

    /// Return true when the EDID input type is compatible with the output port.
    ///
    /// This follows libgfxinit's distinction: VGA is analog, all other current
    /// ports are treated as digital sinks.
    pub fn compatible_with_port(&self, port: Port) -> bool {
        matches!(port, Port::Vga) == self.is_analog()
    }

    /// Return true when descriptor 1 contains a usable detailed timing.
    pub fn has_preferred_mode(&self) -> bool {
        detailed_timing_present(descriptor(self.raw, 0))
    }

    /// Convert descriptor 1 into a display mode.
    pub fn preferred_mode(&self) -> Result<Mode, GmaError> {
        if !self.has_preferred_mode() {
            return Err(GmaError::ModeUnavailable);
        }
        mode_from_dtd(descriptor(self.raw, 0))
    }

    /// Return all valid detailed timing modes from the base block.
    pub fn detailed_modes(&self) -> Vec<Mode, DESCRIPTOR_COUNT> {
        let mut modes = Vec::new();
        let mut i = 0;
        while i < DESCRIPTOR_COUNT {
            let dtd = descriptor(self.raw, i);
            if detailed_timing_present(dtd)
                && let Ok(mode) = mode_from_dtd(dtd)
            {
                let _ = modes.push(mode);
            }
            i += 1;
        }
        modes
    }

    /// Return modes advertised by the EDID established timing bitmaps.
    pub fn established_modes(&self) -> Vec<Mode, 17> {
        let mut modes = Vec::new();
        let bits = u32::from(self.raw[ESTABLISHED_TIMINGS_1])
            | (u32::from(self.raw[ESTABLISHED_TIMINGS_2]) << 8)
            | (u32::from(self.raw[ESTABLISHED_TIMINGS_MFG] & 0x80) << 9);
        let mut i = 0;
        while i < ESTABLISHED_MODES.len() {
            if (bits & (1 << i)) != 0 {
                let _ = modes.push(ESTABLISHED_MODES[i]);
            }
            i += 1;
        }
        modes
    }

    /// Return known DMT modes derived from EDID standard timing entries.
    ///
    /// Like Linux's `drm_mode_std()`, the width, aspect, and refresh are decoded
    /// from the EDID bytes. fstart deliberately returns only modes present in
    /// its built-in DMT subset; GTF/CVT synthesis and a complete DMT catalog are
    /// deferred.
    pub fn standard_modes(&self) -> Vec<Mode, STANDARD_TIMING_COUNT> {
        let mut modes = Vec::new();
        for entry in self.raw[STANDARD_TIMINGS..STANDARD_TIMINGS + STANDARD_TIMING_COUNT * 2]
            .as_chunks::<2>()
            .0
        {
            if let Some(mode) = standard_timing_mode(entry[0], entry[1], self.raw[19]) {
                let _ = modes.push(mode);
            }
        }
        modes
    }

    /// Return the first monitor range limits descriptor in the base block.
    pub fn monitor_range(&self) -> Option<MonitorRange> {
        let mut i = 0;
        while i < DESCRIPTOR_COUNT {
            if let Some(range) = monitor_range_descriptor(descriptor(self.raw, i), self.raw[19]) {
                return Some(range);
            }
            i += 1;
        }
        None
    }

    /// Return all base-block modes in libgfxinit/Linux priority: detailed,
    /// standard, then established timings. Duplicate active-size/refresh
    /// entries are suppressed so detailed timings win over inferred DMT modes.
    pub fn base_modes(&self) -> Vec<Mode, MAX_BASE_MODES> {
        let mut modes = Vec::new();
        for mode in self.detailed_modes() {
            push_unique_mode(&mut modes, mode);
        }
        for mode in self.standard_modes() {
            push_unique_mode(&mut modes, mode);
        }
        for mode in self.established_modes() {
            push_unique_mode(&mut modes, mode);
        }
        modes
    }

    /// Return base-block modes filtered by the optional EDID monitor range.
    ///
    /// This helper is used by `PreferredMode::Edid` after live DDC probing.
    pub fn filtered_modes(&self) -> Vec<Mode, MAX_BASE_MODES> {
        let range = self.monitor_range();
        let mut modes = Vec::new();
        for mode in self.base_modes() {
            if range.map(|r| r.contains_mode(mode)).unwrap_or(true) {
                push_unique_mode(&mut modes, mode);
            }
        }
        modes
    }

    /// Return all modes from the base block plus already-read extension blocks.
    ///
    /// Extension detailed timings are appended after base-block candidates,
    /// matching firmware probing preference for the base preferred timing while
    /// still exposing additional CEA/CTA timings from extension blocks.
    pub fn modes_with_extensions(
        &self,
        extensions: &[[u8; EDID_BLOCK_LEN]],
    ) -> Vec<Mode, MAX_EDID_MODES> {
        let range = self.monitor_range();
        let mut modes = Vec::new();
        for mode in self.base_modes() {
            if range.map(|r| r.contains_mode(mode)).unwrap_or(true) {
                push_unique_mode(&mut modes, mode);
            }
        }
        for extension in extensions.iter().filter(|block| extension_is_valid(block)) {
            for mode in extension_modes(extension) {
                if range.map(|r| r.contains_mode(mode)).unwrap_or(true) {
                    push_unique_mode(&mut modes, mode);
                }
            }
        }
        modes
    }

    /// Return the first EDID/range candidate mode in libgfxinit/Linux priority:
    /// preferred detailed timing, then other detailed timings, standard timings,
    /// and finally established timings, after duplicate and range filtering.
    ///
    /// This does not imply the current hardware path can drive the mode; callers
    /// must still apply generation, output, framebuffer, and scaler constraints.
    pub fn first_candidate_mode(&self) -> Result<Mode, GmaError> {
        self.filtered_modes()
            .first()
            .copied()
            .ok_or(GmaError::ModeUnavailable)
    }

    /// Compatibility wrapper for older callers.
    ///
    /// Prefer `first_candidate_mode()` in new code because this helper reports
    /// EDID/range candidates, not fully hardware-supported modes.
    pub fn first_supported_mode(&self) -> Result<Mode, GmaError> {
        self.first_candidate_mode()
    }
}

/// Return whether a raw EDID base block is valid.
pub fn is_valid(raw: &[u8]) -> bool {
    Edid::parse(raw).is_ok()
}

/// Try libgfxinit-style header sanitization on a copied EDID block.
///
/// If exactly six or seven header bytes match, the header is repaired and the
/// repaired block is accepted only if the checksum then validates.
pub fn sanitize(mut raw: [u8; EDID_BLOCK_LEN]) -> Result<[u8; EDID_BLOCK_LEN], GmaError> {
    let score = header_score(&raw);
    if score == 6 || score == 7 {
        raw[..HEADER.len()].copy_from_slice(&HEADER);
    }
    if header_score(&raw) == HEADER.len() && checksum(&raw) == 0 {
        Ok(raw)
    } else {
        Err(GmaError::ModeUnavailable)
    }
}

fn header_score(raw: &[u8; EDID_BLOCK_LEN]) -> usize {
    raw[..HEADER.len()]
        .iter()
        .zip(HEADER.iter())
        .filter(|(actual, expected)| actual == expected)
        .count()
}

fn descriptor(raw: &[u8; EDID_BLOCK_LEN], index: usize) -> &[u8] {
    let start = DESCRIPTOR_1 + index * DESCRIPTOR_LEN;
    &raw[start..start + DESCRIPTOR_LEN]
}

fn checksum(raw: &[u8; EDID_BLOCK_LEN]) -> u8 {
    raw.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

fn detailed_timing_present(dtd: &[u8]) -> bool {
    dtd_present(dtd)
}

fn standard_timing_mode(width_byte: u8, aspect_refresh: u8, edid_revision: u8) -> Option<Mode> {
    if width_byte == 0
        || (width_byte == 0x01 && aspect_refresh == 0x01)
        || (width_byte == 0x20 && aspect_refresh == 0x20)
    {
        return None;
    }
    let mut width = u16::from(width_byte) * 8 + 248;
    let refresh_hz = u16::from(aspect_refresh & 0x3f) + 60;
    let aspect = (aspect_refresh >> 6) & 0x3;
    let mut height = match aspect {
        0 if edid_revision < 3 => width,
        0 => width * 10 / 16,
        1 => width * 3 / 4,
        2 => width * 4 / 5,
        _ => width * 9 / 16,
    };
    fixup_standard_timing_1366x768(&mut width, &mut height, refresh_hz);
    dmt_mode_for_standard_timing(width, height, refresh_hz)
}

/// Return modes advertised by a validated EDID extension block.
pub fn extension_modes(block: &[u8; EDID_BLOCK_LEN]) -> Vec<Mode, 6> {
    let mut modes = Vec::new();
    if !extension_is_valid(block) || block[0] != CEA_EXTENSION_TAG {
        return modes;
    }
    let dtd_start = block[CEA_DTD_START] as usize;
    if !(CEA_DATA_BLOCK_START..EDID_BLOCK_LEN).contains(&dtd_start) {
        return modes;
    }
    let mut offset = dtd_start;
    while offset + DESCRIPTOR_LEN < EDID_BLOCK_LEN {
        let dtd = &block[offset..offset + DESCRIPTOR_LEN];
        if detailed_timing_present(dtd)
            && let Ok(mode) = mode_from_dtd(dtd)
        {
            push_unique_mode(&mut modes, mode);
        }
        offset += DESCRIPTOR_LEN;
    }
    modes
}

/// Return true when an EDID extension block checksum is valid.
pub fn extension_is_valid(block: &[u8; EDID_BLOCK_LEN]) -> bool {
    checksum(block) == 0
}

fn monitor_range_descriptor(desc: &[u8], edid_revision: u8) -> Option<MonitorRange> {
    if desc.len() < DESCRIPTOR_LEN
        || desc[0] != 0
        || desc[1] != 0
        || desc[2] != 0
        || desc[3] != 0xfd
    {
        return None;
    }

    let offset = if edid_revision >= 4 { desc[4] } else { 0 };
    // EDID 1.4 uses byte 4 as +255 offset flags for vfreq/hfreq min/max.
    // Other range-type details (GTF2/CVT synthesis, precision pixel clocks)
    // are intentionally not interpreted by this pure data slice.
    let min_vrefresh_hz = u16::from(desc[5]) + if (offset & 0x01) != 0 { 255 } else { 0 };
    let max_vrefresh_hz = u16::from(desc[6]) + if (offset & 0x02) != 0 { 255 } else { 0 };
    let min_hfreq_khz = u16::from(desc[7]) + if (offset & 0x04) != 0 { 255 } else { 0 };
    let max_hfreq_khz = u16::from(desc[8]) + if (offset & 0x08) != 0 { 255 } else { 0 };

    Some(MonitorRange {
        min_vrefresh_hz,
        max_vrefresh_hz,
        min_hfreq_khz,
        max_hfreq_khz,
        max_pixel_clock_mhz: if desc[9] == 0 || desc[9] == 255 {
            None
        } else {
            Some(u16::from(desc[9]) * 10)
        },
    })
}

fn dmt_mode_for_standard_timing(width: u16, height: u16, refresh_hz: u16) -> Option<Mode> {
    let mut first = None;
    for mode in all_dmt_modes().filter(|mode| {
        mode.hdisplay == width && mode.vdisplay == height && mode_refresh_hz(*mode) == refresh_hz
    }) {
        if first.is_none() {
            first = Some(mode);
        }
        if !is_reduced_blanking(mode) {
            return Some(mode);
        }
    }
    first
}

fn all_dmt_modes() -> impl Iterator<Item = Mode> {
    ESTABLISHED_MODES
        .iter()
        .chain(ADDITIONAL_DMT_MODES.iter())
        .copied()
}

fn fixup_standard_timing_1366x768(width: &mut u16, height: &mut u16, refresh_hz: u16) {
    if refresh_hz == 60
        && ((*width == 1360 && *height == 765) || (*width == 1368 && *height == 769))
    {
        *width = 1366;
        *height = 768;
    }
}

fn is_reduced_blanking(mode: Mode) -> bool {
    mode.htotal.saturating_sub(mode.hdisplay) <= 160
}

fn push_unique_mode<const N: usize>(modes: &mut Vec<Mode, N>, mode: Mode) {
    if !modes.iter().any(|existing| same_mode_key(*existing, mode)) {
        let _ = modes.push(mode);
    }
}

fn same_mode_key(a: Mode, b: Mode) -> bool {
    a.hdisplay == b.hdisplay
        && a.vdisplay == b.vdisplay
        && mode_refresh_hz(a) == mode_refresh_hz(b)
        && a.flags.contains(ModeFlags::INTERLACE) == b.flags.contains(ModeFlags::INTERLACE)
}

fn mode_refresh_hz(mode: Mode) -> u16 {
    let pixels = u32::from(mode.htotal) * u32::from(mode.vtotal);
    if pixels == 0 {
        return 0;
    }
    ((mode.pixel_clock_khz * 1000 + pixels / 2) / pixels) as u16
}

fn horizontal_frequency_khz(mode: Mode) -> u32 {
    if mode.htotal == 0 {
        return 0;
    }
    (mode.pixel_clock_khz + u32::from(mode.htotal) / 2) / u32::from(mode.htotal)
}

const PHSYNC_PVSYNC: ModeFlags = ModeFlags::PHSYNC.union(ModeFlags::PVSYNC);
const PHSYNC_NVSYNC: ModeFlags = ModeFlags::PHSYNC.union(ModeFlags::NVSYNC);
const NHSYNC_NVSYNC: ModeFlags = ModeFlags::NHSYNC.union(ModeFlags::NVSYNC);
const NHSYNC_PVSYNC: ModeFlags = ModeFlags::NHSYNC.union(ModeFlags::PVSYNC);

const ESTABLISHED_MODES: [Mode; 17] = [
    Mode {
        hdisplay: 800,
        hsync_start: 840,
        hsync_end: 968,
        htotal: 1056,
        vdisplay: 600,
        vsync_start: 601,
        vsync_end: 605,
        vtotal: 628,
        pixel_clock_khz: 40_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 800,
        hsync_start: 824,
        hsync_end: 896,
        htotal: 1024,
        vdisplay: 600,
        vsync_start: 601,
        vsync_end: 603,
        vtotal: 625,
        pixel_clock_khz: 36_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 640,
        hsync_start: 656,
        hsync_end: 720,
        htotal: 840,
        vdisplay: 480,
        vsync_start: 481,
        vsync_end: 484,
        vtotal: 500,
        pixel_clock_khz: 31_500,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 640,
        hsync_start: 664,
        hsync_end: 704,
        htotal: 832,
        vdisplay: 480,
        vsync_start: 489,
        vsync_end: 492,
        vtotal: 520,
        pixel_clock_khz: 31_500,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 640,
        hsync_start: 704,
        hsync_end: 768,
        htotal: 864,
        vdisplay: 480,
        vsync_start: 483,
        vsync_end: 486,
        vtotal: 525,
        pixel_clock_khz: 30_240,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 640,
        hsync_start: 656,
        hsync_end: 752,
        htotal: 800,
        vdisplay: 480,
        vsync_start: 490,
        vsync_end: 492,
        vtotal: 525,
        pixel_clock_khz: 25_175,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 720,
        hsync_start: 738,
        hsync_end: 846,
        htotal: 900,
        vdisplay: 400,
        vsync_start: 421,
        vsync_end: 423,
        vtotal: 449,
        pixel_clock_khz: 35_500,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 720,
        hsync_start: 738,
        hsync_end: 846,
        htotal: 900,
        vdisplay: 400,
        vsync_start: 412,
        vsync_end: 414,
        vtotal: 449,
        pixel_clock_khz: 28_320,
        flags: NHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1296,
        hsync_end: 1440,
        htotal: 1688,
        vdisplay: 1024,
        vsync_start: 1025,
        vsync_end: 1028,
        vtotal: 1066,
        pixel_clock_khz: 135_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1024,
        hsync_start: 1040,
        hsync_end: 1136,
        htotal: 1312,
        vdisplay: 768,
        vsync_start: 769,
        vsync_end: 772,
        vtotal: 800,
        pixel_clock_khz: 78_750,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1024,
        hsync_start: 1048,
        hsync_end: 1184,
        htotal: 1328,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 777,
        vtotal: 806,
        pixel_clock_khz: 75_000,
        flags: NHSYNC_NVSYNC,
    },
    Mode::XGA_1024X768_60,
    Mode {
        hdisplay: 1024,
        hsync_start: 1032,
        hsync_end: 1208,
        htotal: 1264,
        vdisplay: 768,
        vsync_start: 768,
        vsync_end: 776,
        vtotal: 817,
        pixel_clock_khz: 44_900,
        flags: PHSYNC_PVSYNC.union(ModeFlags::INTERLACE),
    },
    Mode {
        hdisplay: 832,
        hsync_start: 864,
        hsync_end: 928,
        htotal: 1152,
        vdisplay: 624,
        vsync_start: 625,
        vsync_end: 628,
        vtotal: 667,
        pixel_clock_khz: 57_284,
        flags: NHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 800,
        hsync_start: 816,
        hsync_end: 896,
        htotal: 1056,
        vdisplay: 600,
        vsync_start: 601,
        vsync_end: 604,
        vtotal: 625,
        pixel_clock_khz: 49_500,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 800,
        hsync_start: 856,
        hsync_end: 976,
        htotal: 1040,
        vdisplay: 600,
        vsync_start: 637,
        vsync_end: 643,
        vtotal: 666,
        pixel_clock_khz: 50_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1152,
        hsync_start: 1216,
        hsync_end: 1344,
        htotal: 1600,
        vdisplay: 864,
        vsync_start: 865,
        vsync_end: 868,
        vtotal: 900,
        pixel_clock_khz: 108_000,
        flags: PHSYNC_PVSYNC,
    },
];

// Linux `drm_dmt_modes` subset beyond the established timing bitmap modes
// above. This covers common standard-timing EDID entries without implementing
// GTF/CVT synthesis or the complete DMT catalog yet.
const ADDITIONAL_DMT_MODES: &[Mode] = &[
    Mode {
        hdisplay: 640,
        hsync_start: 672,
        hsync_end: 736,
        htotal: 832,
        vdisplay: 400,
        vsync_start: 401,
        vsync_end: 404,
        vtotal: 445,
        pixel_clock_khz: 31_500,
        flags: NHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 848,
        hsync_start: 864,
        hsync_end: 976,
        htotal: 1088,
        vdisplay: 480,
        vsync_start: 486,
        vsync_end: 494,
        vtotal: 517,
        pixel_clock_khz: 33_750,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1024,
        hsync_start: 1072,
        hsync_end: 1168,
        htotal: 1376,
        vdisplay: 768,
        vsync_start: 769,
        vsync_end: 772,
        vtotal: 808,
        pixel_clock_khz: 94_500,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1390,
        hsync_end: 1430,
        htotal: 1650,
        vdisplay: 720,
        vsync_start: 725,
        vsync_end: 730,
        vtotal: 750,
        pixel_clock_khz: 74_250,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1328,
        hsync_end: 1360,
        htotal: 1440,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 778,
        vtotal: 790,
        pixel_clock_khz: 68_250,
        flags: PHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1344,
        hsync_end: 1472,
        htotal: 1664,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 778,
        vtotal: 798,
        pixel_clock_khz: 79_500,
        flags: NHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1328,
        hsync_end: 1360,
        htotal: 1440,
        vdisplay: 800,
        vsync_start: 803,
        vsync_end: 809,
        vtotal: 823,
        pixel_clock_khz: 71_000,
        flags: PHSYNC_NVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1352,
        hsync_end: 1480,
        htotal: 1680,
        vdisplay: 800,
        vsync_start: 803,
        vsync_end: 809,
        vtotal: 831,
        pixel_clock_khz: 83_500,
        flags: NHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1376,
        hsync_end: 1488,
        htotal: 1800,
        vdisplay: 960,
        vsync_start: 961,
        vsync_end: 964,
        vtotal: 1000,
        pixel_clock_khz: 108_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1280,
        hsync_start: 1328,
        hsync_end: 1440,
        htotal: 1688,
        vdisplay: 1024,
        vsync_start: 1025,
        vsync_end: 1028,
        vtotal: 1066,
        pixel_clock_khz: 108_000,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1360,
        hsync_start: 1424,
        hsync_end: 1536,
        htotal: 1792,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 777,
        vtotal: 795,
        pixel_clock_khz: 85_500,
        flags: PHSYNC_PVSYNC,
    },
    Mode {
        hdisplay: 1366,
        hsync_start: 1436,
        hsync_end: 1579,
        htotal: 1792,
        vdisplay: 768,
        vsync_start: 771,
        vsync_end: 774,
        vtotal: 798,
        pixel_clock_khz: 85_500,
        flags: PHSYNC_PVSYNC,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::ModeFlags;

    fn xga_edid(digital: bool) -> [u8; EDID_BLOCK_LEN] {
        let mut edid = [0u8; EDID_BLOCK_LEN];
        edid[..HEADER.len()].copy_from_slice(&HEADER);
        edid[18] = 1;
        edid[19] = 4;
        edid[INPUT] = if digital { INPUT_DIGITAL } else { 0 };
        edid[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].copy_from_slice(&[
            0x64, 0x19, // 65.00 MHz
            0x00, 0x40, 0x41, // hactive 1024, hblank 320
            0x00, 0x26, 0x30, // vactive 768, vblank 38
            0x18, 0x88, 0x36, 0x00, // sync offsets/widths
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // negative syncs
        ]);
        fix_checksum(&mut edid);
        edid
    }

    fn fix_checksum(edid: &mut [u8; EDID_BLOCK_LEN]) {
        edid[127] = 0;
        edid[127] = 0u8.wrapping_sub(edid[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
    }

    fn standard_timing(width: u16, aspect: u8, refresh_hz: u8) -> [u8; 2] {
        [
            ((width - 248) / 8) as u8,
            (aspect << 6) | ((refresh_hz - 60) & 0x3f),
        ]
    }

    #[test]
    fn validates_and_extracts_preferred_mode() {
        let raw = xga_edid(false);
        let edid = Edid::parse(&raw).unwrap();
        assert!(edid.has_preferred_mode());
        assert!(edid.compatible_with_port(Port::Vga));
        assert!(!edid.compatible_with_port(Port::Lvds));
        let mode = edid.preferred_mode().unwrap();
        assert_eq!(
            (mode.hdisplay, mode.vdisplay, mode.pixel_clock_khz),
            (1024, 768, 65_000)
        );
        assert!(mode.flags.contains(ModeFlags::NHSYNC));
        assert!(mode.flags.contains(ModeFlags::NVSYNC));
    }

    #[test]
    fn exposes_input_type_and_extension_count_for_probe_logic() {
        let mut raw = xga_edid(true);
        raw[EXTENSION_COUNT] = 2;
        fix_checksum(&mut raw);
        let edid = Edid::parse(&raw).unwrap();
        assert!(edid.is_digital());
        assert!(!edid.is_analog());
        assert_eq!(edid.extension_count(), 2);
        assert!(edid.compatible_with_port(Port::HdmiA));
        assert!(edid.compatible_with_port(Port::DpA));
    }

    #[test]
    fn digital_edid_is_not_vga_compatible() {
        let raw = xga_edid(true);
        let edid = Edid::parse(&raw).unwrap();
        assert!(!edid.compatible_with_port(Port::Vga));
        assert!(edid.compatible_with_port(Port::Lvds));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut raw = xga_edid(false);
        raw[30] ^= 1;
        assert_eq!(Edid::parse(&raw).err(), Some(GmaError::ModeUnavailable));
        assert!(!is_valid(&raw));
    }

    #[test]
    fn sanitize_repairs_near_match_header_only_when_checksum_matches() {
        let good = xga_edid(false);
        let mut raw = good;
        raw[0] = 0xaa;
        let repaired = sanitize(raw).unwrap();
        assert_eq!(repaired, good);

        let mut too_bad = good;
        too_bad[0] = 0xaa;
        too_bad[1] = 0xbb;
        too_bad[2] = 0xcc;
        assert_eq!(sanitize(too_bad).err(), Some(GmaError::ModeUnavailable));
    }

    #[test]
    fn empty_first_descriptor_has_no_preferred_mode() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        fix_checksum(&mut raw);
        let edid = Edid::parse(&raw).unwrap();
        assert!(!edid.has_preferred_mode());
        assert_eq!(edid.preferred_mode().err(), Some(GmaError::ModeUnavailable));
    }

    #[test]
    fn established_timing_bits_follow_linux_order() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[ESTABLISHED_TIMINGS_1] = 1 << 5; // 640x480@60
        raw[ESTABLISHED_TIMINGS_2] = 1 << 3; // 1024x768@60
        fix_checksum(&mut raw);
        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.established_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!((modes[0].hdisplay, modes[0].vdisplay), (640, 480));
        assert_eq!(modes[0].pixel_clock_khz, 25_175);
        assert_eq!(modes[1], Mode::XGA_1024X768_60);
    }

    #[test]
    fn standard_timings_decode_known_dmt_modes_without_gtf_or_cvt() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&standard_timing(1024, 1, 60));
        raw[STANDARD_TIMINGS + 2..STANDARD_TIMINGS + 4]
            .copy_from_slice(&standard_timing(1280, 3, 60));
        raw[STANDARD_TIMINGS + 4..STANDARD_TIMINGS + 6]
            .copy_from_slice(&standard_timing(1400, 1, 60));
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.standard_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0], Mode::XGA_1024X768_60);
        assert_eq!((modes[1].hdisplay, modes[1].vdisplay), (1280, 720));
        assert_eq!(modes[1].pixel_clock_khz, 74_250);
        assert_eq!(edid.base_modes(), modes);
        assert_eq!(edid.first_candidate_mode(), Ok(Mode::XGA_1024X768_60));
        assert_eq!(edid.first_supported_mode(), edid.first_candidate_mode());
    }

    #[test]
    fn standard_timing_rejects_bad_0x2020_sentinel() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&[0x20, 0x20]);
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        assert!(edid.standard_modes().is_empty());
    }

    #[test]
    fn standard_timing_fixup_resolves_1366x768_dmt_mode() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&standard_timing(1360, 3, 60));
        raw[STANDARD_TIMINGS + 2..STANDARD_TIMINGS + 4]
            .copy_from_slice(&standard_timing(1368, 3, 60));
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.standard_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!((modes[0].hdisplay, modes[0].vdisplay), (1366, 768));
        assert_eq!((modes[1].hdisplay, modes[1].vdisplay), (1366, 768));
        assert_eq!(modes[0].pixel_clock_khz, 85_500);
    }

    #[test]
    fn standard_timing_prefers_non_reduced_blanking_dmt_variant() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&standard_timing(1280, 2, 60));
        raw[STANDARD_TIMINGS + 2..STANDARD_TIMINGS + 4]
            .copy_from_slice(&standard_timing(1280, 0, 60));
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.standard_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!((modes[0].hdisplay, modes[0].vdisplay), (1280, 1024));
        assert_eq!(modes[0].pixel_clock_khz, 108_000);
        assert_eq!((modes[1].hdisplay, modes[1].vdisplay), (1280, 800));
        assert_eq!(modes[1].pixel_clock_khz, 83_500);
        assert!(!is_reduced_blanking(modes[1]));
    }

    #[test]
    fn base_modes_deduplicate_and_preserve_priority() {
        let mut raw = xga_edid(false);
        raw[ESTABLISHED_TIMINGS_2] = 1 << 3; // duplicate 1024x768@60
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&standard_timing(1024, 1, 60));
        raw[STANDARD_TIMINGS + 2..STANDARD_TIMINGS + 4]
            .copy_from_slice(&standard_timing(1280, 3, 60));
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.base_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!((modes[0].hdisplay, modes[0].vdisplay), (1024, 768));
        assert_eq!(modes[0].pixel_clock_khz, 65_000); // detailed timing wins
        assert_eq!((modes[1].hdisplay, modes[1].vdisplay), (1280, 720));
    }

    #[test]
    fn filtered_modes_apply_monitor_range_after_deduplication() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[STANDARD_TIMINGS..STANDARD_TIMINGS + 2].copy_from_slice(&standard_timing(1024, 1, 60));
        raw[STANDARD_TIMINGS + 2..STANDARD_TIMINGS + 4]
            .copy_from_slice(&standard_timing(1280, 0, 60));
        let desc = &mut raw[DESCRIPTOR_1 + DESCRIPTOR_LEN..DESCRIPTOR_1 + 2 * DESCRIPTOR_LEN];
        desc.copy_from_slice(&[
            0x00, 0x00, 0x00, 0xfd, 0x00, 50, 75, 30, 80, 7, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.filtered_modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0], Mode::XGA_1024X768_60);
        assert_eq!(edid.first_supported_mode(), Ok(Mode::XGA_1024X768_60));
    }

    #[test]
    fn manufacturer_established_timing_bit_selects_1152x864() {
        let mut raw = xga_edid(false);
        raw[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN].fill(0);
        raw[ESTABLISHED_TIMINGS_MFG] = 0x80;
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let modes = edid.established_modes();
        assert_eq!(modes.len(), 1);
        assert_eq!((modes[0].hdisplay, modes[0].vdisplay), (1152, 864));
        assert_eq!(modes[0].pixel_clock_khz, 108_000);
    }

    #[test]
    fn cea_extension_detailed_timings_are_ranked_after_base_modes() {
        let base = xga_edid(true);
        let edid = Edid::parse(&base).unwrap();
        let mut cea = [0u8; EDID_BLOCK_LEN];
        cea[0] = CEA_EXTENSION_TAG;
        cea[1] = 3;
        cea[CEA_DTD_START] = 4;
        cea[4..22].copy_from_slice(&[
            0x01, 0x1d, 0x00, 0x72, 0x51, 0xd0, 0x1e, 0x20, 0x6e, 0x28, 0x55, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x1e,
        ]);
        cea[127] = 0u8.wrapping_sub(cea[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));

        let ext_modes = extension_modes(&cea);
        assert_eq!(ext_modes.len(), 1);
        assert_eq!((ext_modes[0].hdisplay, ext_modes[0].vdisplay), (1280, 720));

        let modes = edid.modes_with_extensions(&[cea]);
        assert_eq!(modes[0], Mode::XGA_1024X768_60);
        assert!(
            modes
                .iter()
                .any(|mode| (mode.hdisplay, mode.vdisplay) == (1280, 720))
        );
    }

    #[test]
    fn invalid_extension_checksum_is_ignored() {
        let base = xga_edid(true);
        let edid = Edid::parse(&base).unwrap();
        let mut cea = [0u8; EDID_BLOCK_LEN];
        cea[0] = CEA_EXTENSION_TAG;
        cea[CEA_DTD_START] = 4;
        cea[4..22].copy_from_slice(&base[DESCRIPTOR_1..DESCRIPTOR_1 + DESCRIPTOR_LEN]);
        assert!(!extension_is_valid(&cea));
        assert!(extension_modes(&cea).is_empty());
        assert_eq!(edid.modes_with_extensions(&[cea]), edid.filtered_modes());
    }

    #[test]
    fn monitor_range_descriptor_is_extracted() {
        let mut raw = xga_edid(false);
        let desc = &mut raw[DESCRIPTOR_1 + DESCRIPTOR_LEN..DESCRIPTOR_1 + 2 * DESCRIPTOR_LEN];
        desc.copy_from_slice(&[
            0x00, 0x00, 0x00, 0xfd, 0x00, 50, 75, 30, 80, 14, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        let range = edid.monitor_range().unwrap();
        assert_eq!(
            range,
            MonitorRange {
                min_vrefresh_hz: 50,
                max_vrefresh_hz: 75,
                min_hfreq_khz: 30,
                max_hfreq_khz: 80,
                max_pixel_clock_mhz: Some(140),
            }
        );
        assert!(range.contains_mode(Mode::XGA_1024X768_60));

        let slack_range = MonitorRange {
            min_vrefresh_hz: 50,
            max_vrefresh_hz: 200,
            min_hfreq_khz: 30,
            max_hfreq_khz: 200,
            max_pixel_clock_mhz: Some(140),
        };
        let mut within_slack = Mode::XGA_1024X768_60;
        within_slack.pixel_clock_khz = 145_001;
        assert!(slack_range.contains_mode(within_slack));

        let mut too_high_clock = Mode::XGA_1024X768_60;
        too_high_clock.pixel_clock_khz = 145_002;
        assert!(!slack_range.contains_mode(too_high_clock));
    }

    #[test]
    fn monitor_range_descriptor_applies_edid_14_offset_flags() {
        let mut raw = xga_edid(false);
        let desc = &mut raw[DESCRIPTOR_1 + DESCRIPTOR_LEN..DESCRIPTOR_1 + 2 * DESCRIPTOR_LEN];
        desc.copy_from_slice(&[
            0x00, 0x00, 0x00, 0xfd, 0x0f, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        assert_eq!(
            edid.monitor_range().unwrap(),
            MonitorRange {
                min_vrefresh_hz: 256,
                max_vrefresh_hz: 257,
                min_hfreq_khz: 258,
                max_hfreq_khz: 259,
                max_pixel_clock_mhz: None,
            }
        );
    }

    #[test]
    fn monitor_range_pixel_clock_zero_and_255_are_unspecified() {
        let mut raw = xga_edid(false);
        let desc = &mut raw[DESCRIPTOR_1 + DESCRIPTOR_LEN..DESCRIPTOR_1 + 2 * DESCRIPTOR_LEN];
        desc.copy_from_slice(&[
            0x00, 0x00, 0x00, 0xfd, 0x00, 50, 75, 30, 80, 255, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        fix_checksum(&mut raw);

        let edid = Edid::parse(&raw).unwrap();
        assert_eq!(edid.monitor_range().unwrap().max_pixel_clock_mhz, None);
    }
}
