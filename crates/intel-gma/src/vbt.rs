//! Minimal zero-copy Intel VBT/BDB parser.

use serde::{Deserialize, Serialize};

use crate::error::GmaError;
use crate::gmbus::GmbusPin;
use crate::mode::{Mode, ModeFlags};
use crate::panel::{
    LfpBacklightInfo, LfpFpTiming, LfpPanelMetadata, LfpPowerFeatures, LvdsPanelOptions,
};

const VBT_SIGNATURE: &[u8; 4] = b"$VBT";
const BDB_GENERAL_DEFINITIONS: u8 = 2;
const BDB_LVDS_OPTIONS: u8 = 40;
const BDB_LVDS_LFP_DATA_PTRS: u8 = 41;
const BDB_LVDS_LFP_DATA: u8 = 42;
const BDB_LVDS_BACKLIGHT: u8 = 43;
const BDB_LVDS_POWER: u8 = 44;

/// Parsed VBT header and borrowed byte slice.
#[derive(Debug, Clone, Copy)]
pub struct Vbt<'a> {
    bytes: &'a [u8],
    bdb_offset: usize,
    bdb_size: usize,
    bdb_header_size: usize,
}

/// Borrowed BDB block.
#[derive(Debug, Clone, Copy)]
pub struct BdbBlock<'a> {
    /// BDB block id.
    pub id: u8,
    /// BDB-relative offset of this block's payload.
    pub payload_offset: usize,
    /// BDB block payload bytes.
    pub data: &'a [u8],
}

/// Data-only metadata from VBT block 2 (`BDB_GENERAL_DEFINITIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralDefinitionsMetadata {
    /// Raw `crt_ddc_gmbus_pin` byte from the VBT.
    pub raw_crt_ddc_gmbus_pin: u8,
    /// Modeled legacy GMBUS pin, when the raw VBT value names one fstart models.
    pub crt_ddc_pin: Option<GmbusPin>,
    /// DPMS non-ACPI support bit.
    pub dpms_non_acpi: bool,
    /// VBT asks firmware to skip boot CRT detection.
    pub skip_boot_crt_detect: bool,
    /// DPMS AIM bit.
    pub dpms_aim: bool,
    /// Raw boot display bitfield bytes.
    pub boot_display: [u8; 2],
    /// Child-device entry size declared by the VBT.
    pub child_device_size: u8,
}

/// Iterator over BDB blocks.
pub struct BdbBlocks<'a> {
    bytes: &'a [u8],
    base_offset: usize,
    offset: usize,
}

impl<'a> Iterator for BdbBlocks<'a> {
    type Item = BdbBlock<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 3 > self.bytes.len() {
            return None;
        }
        let id = self.bytes[self.offset];
        let len =
            u16::from_le_bytes([self.bytes[self.offset + 1], self.bytes[self.offset + 2]]) as usize;
        let start = self.offset + 3;
        let end = start.checked_add(len)?;
        if end > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }
        self.offset = end;
        Some(BdbBlock {
            id,
            payload_offset: self.base_offset + start,
            data: &self.bytes[start..end],
        })
    }
}

impl<'a> Vbt<'a> {
    /// Parse and validate VBT/BDB headers.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, GmaError> {
        if bytes.len() < 0x20 || &bytes[0..4] != VBT_SIGNATURE {
            return Err(GmaError::VbtInvalid);
        }
        let header_size = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        let bdb_offset = u16::from_le_bytes([bytes[0x16], bytes[0x17]]) as usize;
        let vbt_size = u16::from_le_bytes([bytes[0x18], bytes[0x19]]) as usize;
        if header_size > bytes.len() || vbt_size > bytes.len() || bdb_offset + 0x16 > bytes.len() {
            return Err(GmaError::VbtInvalid);
        }
        if &bytes[bdb_offset..bdb_offset + 4] != b"BIOS" {
            return Err(GmaError::VbtInvalid);
        }
        let bdb_header_size =
            u16::from_le_bytes([bytes[bdb_offset + 0x12], bytes[bdb_offset + 0x13]]) as usize;
        let bdb_size =
            u16::from_le_bytes([bytes[bdb_offset + 0x14], bytes[bdb_offset + 0x15]]) as usize;
        if bdb_header_size < 0x16 || bdb_header_size > bdb_size {
            return Err(GmaError::VbtInvalid);
        }
        if bdb_offset
            .checked_add(bdb_size)
            .ok_or(GmaError::VbtInvalid)?
            > bytes.len()
        {
            return Err(GmaError::VbtInvalid);
        }
        Ok(Self {
            bytes,
            bdb_offset,
            bdb_size,
            bdb_header_size,
        })
    }

    /// Return the VBT byte slice.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Iterate over BDB blocks.
    pub fn blocks(&self) -> BdbBlocks<'a> {
        // Skip BDB header. Header bounds are validated by `parse()` so this
        // cannot panic on malformed header sizes.
        let start = self.bdb_offset + self.bdb_header_size;
        let end = self.bdb_offset + self.bdb_size;
        BdbBlocks {
            bytes: &self.bytes[start..end],
            base_offset: self.bdb_header_size,
            offset: 0,
        }
    }

    /// Find a BDB block by id.
    pub fn block(&self, id: u8) -> Option<BdbBlock<'a>> {
        self.blocks().find(|block| block.id == id)
    }

    /// Parse metadata from the BDB general definitions block.
    ///
    /// This exposes Linux's `crt_ddc_gmbus_pin` source for DDC probing and
    /// does not enable live DDC probing or change connector policy.
    pub fn general_definitions_metadata(&self) -> Option<GeneralDefinitionsMetadata> {
        self.block(BDB_GENERAL_DEFINITIONS)
            .and_then(parse_general_definitions)
    }

    /// Return true when VBT indicates an LVDS/LFP child device or LFP options block.
    pub fn has_lvds(&self) -> bool {
        self.block(BDB_LVDS_OPTIONS).is_some()
            || self
                .block(BDB_GENERAL_DEFINITIONS)
                .map(|block| block.data.windows(4).any(|w| w == b"LFP\0" || w == b"LVDS"))
                .unwrap_or(false)
    }

    /// Try to extract the selected LFP fixed panel mode.
    pub fn lfp_fixed_mode(&self) -> Result<Mode, GmaError> {
        self.lfp_panel_metadata()
            .map(|metadata| metadata.fixed_mode)
    }

    /// Parse safe selected LFP panel metadata from VBT blocks 40-44.
    ///
    /// This mirrors the data dependencies Linux/libgfxinit use to identify the
    /// fixed LVDS panel and collect power/backlight policy, but it does not
    /// program or otherwise apply any of the embedded register values.
    pub fn lfp_panel_metadata(&self) -> Result<LfpPanelMetadata, GmaError> {
        let options_block = self
            .block(BDB_LVDS_OPTIONS)
            .ok_or(GmaError::ModeUnavailable)?;
        let ptrs = self
            .block(BDB_LVDS_LFP_DATA_PTRS)
            .ok_or(GmaError::ModeUnavailable)?;
        let data = self
            .block(BDB_LVDS_LFP_DATA)
            .ok_or(GmaError::ModeUnavailable)?;
        let options = parse_lfp_options(options_block)?;
        let panel_type = options.panel_type.ok_or(GmaError::ModeUnavailable)?;
        let (fixed_mode, fp_timing) = parse_lfp_selected_entry(ptrs, data, panel_type)?;
        Ok(LfpPanelMetadata {
            panel_type,
            fixed_mode,
            options,
            fp_timing,
            backlight: self
                .block(BDB_LVDS_BACKLIGHT)
                .and_then(|block| parse_lfp_backlight(block, panel_type)),
            power: self
                .block(BDB_LVDS_POWER)
                .and_then(parse_lfp_power_features),
        })
    }
}

fn parse_general_definitions(block: BdbBlock<'_>) -> Option<GeneralDefinitionsMetadata> {
    let raw_crt_ddc_gmbus_pin = *block.data.first()?;
    let dpms = *block.data.get(1)?;
    Some(GeneralDefinitionsMetadata {
        raw_crt_ddc_gmbus_pin,
        crt_ddc_pin: GmbusPin::from_legacy_select(raw_crt_ddc_gmbus_pin),
        dpms_non_acpi: (dpms & 0x01) != 0,
        skip_boot_crt_detect: (dpms & 0x02) != 0,
        dpms_aim: (dpms & 0x04) != 0,
        boot_display: [*block.data.get(2)?, *block.data.get(3)?],
        child_device_size: *block.data.get(4)?,
    })
}

fn le_u16(bytes: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(off)?, *bytes.get(off + 1)?]))
}

fn le_u32(bytes: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *bytes.get(off)?,
        *bytes.get(off + 1)?,
        *bytes.get(off + 2)?,
        *bytes.get(off + 3)?,
    ]))
}

fn parse_lfp_options(block: BdbBlock<'_>) -> Result<LvdsPanelOptions, GmaError> {
    let raw_panel_type = *block.data.first().ok_or(GmaError::ModeUnavailable)?;
    let capabilities = *block.data.get(2).unwrap_or(&0);
    Ok(LvdsPanelOptions {
        panel_type: if raw_panel_type <= 0x0f {
            Some(raw_panel_type)
        } else {
            None
        },
        raw_panel_type,
        pfit_mode: capabilities & 0x03,
        pfit_text_mode_enhanced: (capabilities & (1 << 2)) != 0,
        pfit_gfx_mode_enhanced: (capabilities & (1 << 3)) != 0,
        pfit_ratio_auto: (capabilities & (1 << 4)) != 0,
        pixel_dither: (capabilities & (1 << 5)) != 0,
        lvds_edid: (capabilities & (1 << 6)) != 0,
        lvds_panel_channel_bits: le_u32(block.data, 4),
        ssc_bits: le_u16(block.data, 8),
        ssc_freq: le_u16(block.data, 10),
        panel_color_depth: le_u16(block.data, 14),
        dps_panel_type_bits: le_u32(block.data, 16),
        backlight_control_type_bits: le_u32(block.data, 20),
    })
}

fn parse_lfp_selected_entry(
    ptrs: BdbBlock<'_>,
    data: BdbBlock<'_>,
    panel_type: u8,
) -> Result<(Mode, Option<LfpFpTiming>), GmaError> {
    // BDB 41 starts with a sub-record count, not a panel-slot count. Linux
    // expects this to be the fixed three table groups
    // (fp_timing+dvo_timing+panel_pnp_id), followed by up to 16 panel-slot
    // pointer records. Offsets are BDB-relative. The selected DVO timing table
    // is an EDID DTD and is the best initial source for a fixed panel mode.
    let sub_record_count = ptrs
        .data
        .first()
        .copied()
        .ok_or(GmaError::ModeUnavailable)?;
    if sub_record_count != 3 || panel_type > 0x0f {
        return Err(GmaError::ModeUnavailable);
    }

    let panel = usize::from(panel_type);
    let record = 1 + panel * 9;
    if ptrs.data.len() < record + 9 {
        return Err(GmaError::ModeUnavailable);
    }
    let fp_offset = le_u16(ptrs.data, record).ok_or(GmaError::ModeUnavailable)? as usize;
    let fp_size = *ptrs.data.get(record + 2).ok_or(GmaError::ModeUnavailable)? as usize;
    let dvo_offset = le_u16(ptrs.data, record + 3).ok_or(GmaError::ModeUnavailable)? as usize;
    let dvo_size = *ptrs.data.get(record + 5).ok_or(GmaError::ModeUnavailable)? as usize;

    if dvo_size < 18 {
        return Err(GmaError::ModeUnavailable);
    }
    let relative = dvo_offset
        .checked_sub(data.payload_offset)
        .ok_or(GmaError::ModeUnavailable)?;
    let dtd = data
        .data
        .get(relative..relative.checked_add(18).ok_or(GmaError::ModeUnavailable)?)
        .ok_or(GmaError::ModeUnavailable)?;
    let fp_timing = parse_lfp_fp_timing(data, fp_offset, fp_size, dvo_offset);
    Ok((mode_from_dtd(dtd)?, fp_timing))
}

fn parse_lfp_fp_timing(
    data: BdbBlock<'_>,
    fp_offset: usize,
    fp_size: usize,
    dvo_offset: usize,
) -> Option<LfpFpTiming> {
    let relative = fp_offset.checked_sub(data.payload_offset)?;
    let gap_to_dvo = dvo_offset.checked_sub(fp_offset)?;
    // Linux treats fp_timing as variable-sized. Some real VBTs leave a common
    // 6-byte pad between the declared FP timing size and the following DVO
    // timing; allow that pad only when the pointer layout proves the bytes are
    // still before DVO. If table values are inconsistent, never extend beyond
    // the DVO pointer and risk interpreting DVO timing bytes as FP registers.
    let available = if gap_to_dvo >= fp_size {
        gap_to_dvo.min(fp_size.saturating_add(6))
    } else {
        gap_to_dvo
    };
    // The first 36 bytes carry the legacy registers; pfit_reg/pfit_reg_val are
    // parsed only when validated bounds prove they are present.
    if available < 36 {
        return None;
    }
    let end = relative.checked_add(available)?;
    let bytes = data.data.get(relative..end)?;
    Some(LfpFpTiming {
        x_res: le_u16(bytes, 0)?,
        y_res: le_u16(bytes, 2)?,
        lvds_reg: le_u32(bytes, 4)?,
        lvds_reg_val: le_u32(bytes, 8)?,
        pp_on_reg: le_u32(bytes, 12)?,
        pp_on_reg_val: le_u32(bytes, 16)?,
        pp_off_reg: le_u32(bytes, 20)?,
        pp_off_reg_val: le_u32(bytes, 24)?,
        pp_cycle_reg: le_u32(bytes, 28)?,
        pp_cycle_reg_val: le_u32(bytes, 32)?,
        pfit_reg: le_u32(bytes, 36).unwrap_or(0),
        pfit_reg_val: le_u32(bytes, 40).unwrap_or(0),
    })
}

fn parse_lfp_backlight(block: BdbBlock<'_>, panel_type: u8) -> Option<LfpBacklightInfo> {
    let entry_size = usize::from(*block.data.first()?);
    if entry_size < 6 {
        return None;
    }
    let start = 1usize.checked_add(usize::from(panel_type).checked_mul(entry_size)?)?;
    let entry = block.data.get(start..start + 6)?;
    Some(LfpBacklightInfo {
        backlight_type: entry[0] & 0x03,
        active_low_pwm: (entry[0] & (1 << 2)) != 0,
        i2c_pin: (entry[0] >> 3) & 0x07,
        i2c_speed: (entry[0] >> 6) & 0x03,
        pwm_freq_hz: le_u16(entry, 1)?,
        min_brightness: entry[3],
        i2c_address: entry[4],
        i2c_command: entry[5],
    })
}

fn parse_lfp_power_features(block: BdbBlock<'_>) -> Option<LfpPowerFeatures> {
    let features = *block.data.first()?;
    Some(LfpPowerFeatures {
        dpst_supported: (features & 0x01) != 0,
        power_conservation_preference: (features >> 1) & 0x07,
        lace_enabled_status: (features & (1 << 5)) != 0,
        lace_supported: (features & (1 << 6)) != 0,
        als_enabled: (features & (1 << 7)) != 0,
    })
}

/// Convert an 18-byte EDID/DTD-style timing descriptor to a mode.
pub fn mode_from_dtd(dtd: &[u8]) -> Result<Mode, GmaError> {
    if dtd.len() < 18 {
        return Err(GmaError::ModeUnavailable);
    }
    let pixel_clock_khz = le_u16(dtd, 0).ok_or(GmaError::ModeUnavailable)? as u32 * 10;
    let hactive = u16::from(dtd[2]) | (u16::from(dtd[4] & 0xf0) << 4);
    let hblank = u16::from(dtd[3]) | (u16::from(dtd[4] & 0x0f) << 8);
    let vactive = u16::from(dtd[5]) | (u16::from(dtd[7] & 0xf0) << 4);
    let vblank = u16::from(dtd[6]) | (u16::from(dtd[7] & 0x0f) << 8);
    let hsync_off = u16::from(dtd[8]) | (u16::from(dtd[11] & 0xc0) << 2);
    let hsync_width = u16::from(dtd[9]) | (u16::from(dtd[11] & 0x30) << 4);
    let vsync_off = u16::from((dtd[10] >> 4) & 0x0f) | (u16::from(dtd[11] & 0x0c) << 2);
    let vsync_width = u16::from(dtd[10] & 0x0f) | (u16::from(dtd[11] & 0x03) << 4);
    // EDID detailed-timing separate-sync polarity bits: bit 1 is horizontal
    // positive, bit 2 is vertical positive (Linux DRM_EDID_PT_HSYNC_POSITIVE /
    // DRM_EDID_PT_VSYNC_POSITIVE). Missing bits mean negative polarity.
    let mut flags = match dtd[17] & 0x06 {
        0x06 => ModeFlags::PHSYNC | ModeFlags::PVSYNC,
        0x04 => ModeFlags::NHSYNC | ModeFlags::PVSYNC,
        0x02 => ModeFlags::PHSYNC | ModeFlags::NVSYNC,
        _ => ModeFlags::NHSYNC | ModeFlags::NVSYNC,
    };
    if (dtd[17] & 0x80) != 0 {
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

/// Return VBT total size when the header is present.
pub fn declared_vbt_size(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 0x0a || &bytes[0..4] != VBT_SIGNATURE {
        return None;
    }
    le_u16(bytes, 0x18).map(u32::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const X61_VBT: &[u8] = include_bytes!("../../../boards/lenovo/x61/data.vbt");
    const FOXCONN_VBT: &[u8] = include_bytes!("../../../boards/foxconn/d41s/data.vbt");

    fn selected_lfp_offsets(bytes: &[u8]) -> (usize, usize, usize, usize, usize) {
        let vbt = Vbt::parse(bytes).unwrap();
        let options = vbt.block(BDB_LVDS_OPTIONS).unwrap();
        let panel = usize::from(bytes[vbt.bdb_offset + options.payload_offset]);
        let ptrs = vbt.block(BDB_LVDS_LFP_DATA_PTRS).unwrap();
        let data = vbt.block(BDB_LVDS_LFP_DATA).unwrap();
        let record = 1 + panel * 9;
        let fp_offset = usize::from(le_u16(ptrs.data, record).unwrap());
        let dvo_offset = usize::from(le_u16(ptrs.data, record + 3).unwrap());
        (
            vbt.bdb_offset + ptrs.payload_offset + record,
            vbt.bdb_offset + data.payload_offset + fp_offset - data.payload_offset,
            vbt.bdb_offset + data.payload_offset + dvo_offset - data.payload_offset,
            fp_offset,
            dvo_offset,
        )
    }

    fn xga_dtd_with_flags(flags: u8) -> [u8; 18] {
        [
            0x64, 0x19, // 65.00 MHz
            0x00, 0x40, 0x41, // hactive 1024, hblank 320
            0x00, 0x26, 0x30, // vactive 768, vblank 38
            0x18, 0x88, 0x36, 0x00, // sync offsets/widths
            0x00, 0x00, 0x00, 0x00, 0x00, flags,
        ]
    }

    #[test]
    fn dtd_sync_polarity_uses_edid_hsync_bit1_vsync_bit2() {
        let vertical_positive = mode_from_dtd(&xga_dtd_with_flags(0x04)).unwrap();
        assert!(vertical_positive.flags.contains(ModeFlags::NHSYNC));
        assert!(vertical_positive.flags.contains(ModeFlags::PVSYNC));
        assert!(!vertical_positive.flags.contains(ModeFlags::PHSYNC));
        assert!(!vertical_positive.flags.contains(ModeFlags::NVSYNC));

        let horizontal_positive = mode_from_dtd(&xga_dtd_with_flags(0x02)).unwrap();
        assert!(horizontal_positive.flags.contains(ModeFlags::PHSYNC));
        assert!(horizontal_positive.flags.contains(ModeFlags::NVSYNC));
        assert!(!horizontal_positive.flags.contains(ModeFlags::NHSYNC));
        assert!(!horizontal_positive.flags.contains(ModeFlags::PVSYNC));
    }

    #[test]
    fn dtd_interlace_bit_sets_mode_flag() {
        let interlaced = mode_from_dtd(&xga_dtd_with_flags(0x80)).unwrap();
        assert!(interlaced.flags.contains(ModeFlags::INTERLACE));
        assert!(interlaced.flags.contains(ModeFlags::NHSYNC));
        assert!(interlaced.flags.contains(ModeFlags::NVSYNC));
    }

    #[test]
    fn real_vbts_expose_selected_lfp_panel_metadata() {
        for bytes in [X61_VBT, FOXCONN_VBT] {
            let vbt = Vbt::parse(bytes).unwrap();
            let metadata = vbt.lfp_panel_metadata().unwrap();
            assert_eq!(metadata.panel_type, 2);
            assert_eq!(metadata.options.panel_type, Some(2));
            assert_eq!(metadata.options.raw_panel_type, 2);
            assert_eq!(metadata.fixed_mode.hdisplay, 1024);
            assert_eq!(metadata.fixed_mode.vdisplay, 768);
            assert_eq!(metadata.fp_timing.unwrap().x_res, 1024);
            assert_eq!(metadata.fp_timing.unwrap().y_res, 768);
            assert!(metadata.backlight.unwrap().is_pwm());
            assert_eq!(metadata.power.unwrap().power_conservation_preference, 4);
        }
    }

    #[test]
    fn real_vbts_expose_general_definitions_crt_ddc_metadata() {
        for bytes in [X61_VBT, FOXCONN_VBT] {
            let metadata = Vbt::parse(bytes)
                .unwrap()
                .general_definitions_metadata()
                .unwrap();
            assert_eq!(metadata.raw_crt_ddc_gmbus_pin, 2);
            assert_eq!(metadata.crt_ddc_pin, Some(GmbusPin::Analog));
            assert_eq!(metadata.boot_display, [0x00, 0x00]);
            assert_ne!(metadata.child_device_size, 0);
        }
    }

    #[test]
    fn general_definitions_rejects_too_short_and_preserves_invalid_ddc_pin() {
        assert_eq!(
            parse_general_definitions(BdbBlock {
                id: BDB_GENERAL_DEFINITIONS,
                payload_offset: 0,
                data: &[2, 0, 0, 0],
            }),
            None
        );

        let metadata = parse_general_definitions(BdbBlock {
            id: BDB_GENERAL_DEFINITIONS,
            payload_offset: 0,
            data: &[7, 0x07, 0x12, 0x34, 0x21],
        })
        .unwrap();
        assert_eq!(metadata.raw_crt_ddc_gmbus_pin, 7);
        assert_eq!(metadata.crt_ddc_pin, None);
        assert!(metadata.dpms_non_acpi);
        assert!(metadata.skip_boot_crt_detect);
        assert!(metadata.dpms_aim);
        assert_eq!(metadata.boot_display, [0x12, 0x34]);
        assert_eq!(metadata.child_device_size, 0x21);
    }

    #[test]
    fn selected_lfp_panel_slot_15_is_valid_when_pointer_tables_fit() {
        let mut bytes = X61_VBT.to_vec();
        let options_offset = {
            let vbt = Vbt::parse(&bytes).unwrap();
            let options = vbt.block(BDB_LVDS_OPTIONS).unwrap();
            vbt.bdb_offset + options.payload_offset
        };
        bytes[options_offset] = 15;

        let metadata = Vbt::parse(&bytes).unwrap().lfp_panel_metadata().unwrap();
        assert_eq!(metadata.panel_type, 15);
        assert_eq!(metadata.options.panel_type, Some(15));
        assert!(metadata.fixed_mode.is_valid());
        let fp_timing = metadata.fp_timing.unwrap();
        assert_eq!(fp_timing.x_res, metadata.fixed_mode.hdisplay);
        assert_eq!(fp_timing.y_res, metadata.fixed_mode.vdisplay);
    }

    #[test]
    fn lfp_fp_timing_parses_pfit_registers_from_common_gap() {
        let mut bytes = X61_VBT.to_vec();
        let (_, fp_abs_offset, _, _, _) = selected_lfp_offsets(&bytes);
        bytes[fp_abs_offset + 36..fp_abs_offset + 40].copy_from_slice(&0x61234u32.to_le_bytes());
        bytes[fp_abs_offset + 40..fp_abs_offset + 44]
            .copy_from_slice(&0xa5a5_5a5au32.to_le_bytes());

        let metadata = Vbt::parse(&bytes).unwrap().lfp_panel_metadata().unwrap();
        let fp_timing = metadata.fp_timing.unwrap();
        assert_eq!(fp_timing.pfit_reg, 0x61234);
        assert_eq!(fp_timing.pfit_reg_val, 0xa5a5_5a5a);
    }

    #[test]
    fn lfp_fp_timing_does_not_read_pfit_registers_from_dvo_data() {
        let mut bytes = X61_VBT.to_vec();
        let (record_abs, fp_abs, original_dvo_abs, fp_offset, _) = selected_lfp_offsets(&bytes);
        let new_dvo_offset = fp_offset + 36;
        let dtd = bytes[original_dvo_abs..original_dvo_abs + 18].to_vec();

        bytes[record_abs + 2] = 46;
        bytes[record_abs + 3..record_abs + 5]
            .copy_from_slice(&(new_dvo_offset as u16).to_le_bytes());
        bytes[fp_abs + 36..fp_abs + 54].copy_from_slice(&dtd);

        let metadata = Vbt::parse(&bytes).unwrap().lfp_panel_metadata().unwrap();
        let fp_timing = metadata.fp_timing.unwrap();
        assert_eq!(fp_timing.pfit_reg, 0);
        assert_eq!(fp_timing.pfit_reg_val, 0);
    }

    #[test]
    fn lfp_fp_timing_caps_common_padding_before_dvo() {
        let mut bytes = X61_VBT.to_vec();
        let (record_abs, fp_abs, _, _, _) = selected_lfp_offsets(&bytes);

        bytes[record_abs + 2] = 40;
        bytes[fp_abs + 46..fp_abs + 50].copy_from_slice(&0xfeed_cafeu32.to_le_bytes());

        let metadata = Vbt::parse(&bytes).unwrap().lfp_panel_metadata().unwrap();
        let fp_timing = metadata.fp_timing.unwrap();
        assert_ne!(fp_timing.pfit_reg, 0xfeed_cafe);
        assert_ne!(fp_timing.pfit_reg_val, 0xfeed_cafe);
    }

    #[test]
    fn selected_lfp_backlight_uses_panel_slot() {
        let x61 = Vbt::parse(X61_VBT).unwrap().lfp_panel_metadata().unwrap();
        let foxconn = Vbt::parse(FOXCONN_VBT)
            .unwrap()
            .lfp_panel_metadata()
            .unwrap();

        assert_eq!(x61.backlight.unwrap().pwm_freq_hz, 165);
        assert_eq!(x61.backlight.unwrap().min_brightness, 0x23);
        assert_eq!(foxconn.backlight.unwrap().pwm_freq_hz, 200);
        assert_eq!(foxconn.backlight.unwrap().min_brightness, 0);
    }

    #[test]
    fn invalid_lfp_panel_type_is_not_selected() {
        let mut bytes = X61_VBT.to_vec();
        let options_offset = {
            let vbt = Vbt::parse(&bytes).unwrap();
            let options = vbt.block(BDB_LVDS_OPTIONS).unwrap();
            vbt.bdb_offset + options.payload_offset
        };
        bytes[options_offset] = 0x10;

        let vbt = Vbt::parse(&bytes).unwrap();
        assert_eq!(vbt.lfp_panel_metadata(), Err(GmaError::ModeUnavailable));
    }
}
