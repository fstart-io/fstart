//! Minimal zero-copy Intel VBT/BDB parser.
//!
//! The on-disk records are declared as `zerocopy` mirrors so decoding is a
//! checked cast over the VBT bytes instead of hand-rolled little-endian reads.

use zerocopy::byteorder::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::error::GmaError;
use crate::gmbus::GmbusPin;
use crate::dtd::{mode_from_dtd, DTD_LEN};
use crate::mode::Mode;
use crate::panel::{
    LfpBacklightInfo, LfpFpTiming, LfpPanelMetadata, LfpPowerFeatures, LvdsPanelOptions,
};

const VBT_SIGNATURE: &[u8; 4] = b"$VBT";
const BDB_SIGNATURE: &[u8; 4] = b"BIOS";
const BDB_GENERAL_DEFINITIONS: u8 = 2;
const BDB_LVDS_OPTIONS: u8 = 40;
const BDB_LVDS_LFP_DATA_PTRS: u8 = 41;
const BDB_LVDS_LFP_DATA: u8 = 42;
const BDB_LVDS_BACKLIGHT: u8 = 43;
const BDB_LVDS_POWER: u8 = 44;

/// VBT header prefix (0x00..0x20).
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct VbtHeader {
    signature: [u8; 4],
    _reserved: [u8; 0x10],
    header_size: U16,
    bdb_offset: U16,
    vbt_size: U16,
    _tail: [u8; 6],
}

/// BDB header (0x00..0x16 relative to the BDB offset).
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct BdbHeader {
    signature: [u8; 4],
    _reserved: [u8; 0x0e],
    header_size: U16,
    bdb_size: U16,
}

/// Three-byte BDB block header.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct BdbBlockHeader {
    id: u8,
    size: U16,
}

/// BDB block 2 general definitions (first five bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct GeneralDefinitionsRaw {
    crt_ddc_gmbus_pin: u8,
    dpms: u8,
    boot_display: [u8; 2],
    child_device_size: u8,
}

/// BDB block 40 LFP options guaranteed prefix (12 bytes).
///
/// Real VBTs are shorter than Linux's modern `bdb_lvds_options` structure (the
/// boards here carry 14 and 16 byte blocks), so the panel colour depth and the
/// trailing DRRS/backlight dwords are read separately and left as `None` when
/// the block ends early.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct LvdsOptionsPrefix {
    panel_type: u8,
    _reserved0: u8,
    capabilities: u8,
    _reserved1: u8,
    lvds_panel_channel_bits: U32,
    ssc_bits: U16,
    ssc_freq: U16,
}

/// Read a little-endian `u16` at `offset` if the slice contains it.
fn read_u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let (value, _) = U16::ref_from_prefix(bytes.get(offset..)?).ok()?;
    Some(value.get())
}

/// Read a little-endian `u32` at `offset` if the slice contains it.
fn read_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let (value, _) = U32::ref_from_prefix(bytes.get(offset..)?).ok()?;
    Some(value.get())
}

/// One nine-byte entry of BDB block 41's panel pointer table.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct LfpDataPtrEntry {
    fp_timing_offset: U16,
    fp_timing_size: u8,
    dvo_timing_offset: U16,
    dvo_timing_size: u8,
    panel_pnp_id_offset: U16,
    panel_pnp_id_size: u8,
}

/// BDB block 42 fixed FP timing registers, guaranteed prefix (36 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct LfpFpTimingPrefix {
    x_res: U16,
    y_res: U16,
    lvds_reg: U32,
    lvds_reg_val: U32,
    pp_on_reg: U32,
    pp_on_reg_val: U32,
    pp_off_reg: U32,
    pp_off_reg_val: U32,
    pp_cycle_reg: U32,
    pp_cycle_reg_val: U32,
}

/// BDB block 43 per-panel backlight entry (6 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct LfpBacklightRaw {
    flags: u8,
    pwm_freq_hz: U16,
    min_brightness: u8,
    i2c_address: u8,
    i2c_command: u8,
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        let (header, _) = BdbBlockHeader::ref_from_prefix(self.bytes.get(self.offset..)?).ok()?;
        let len = usize::from(header.size.get());
        let start = self.offset + size_of::<BdbBlockHeader>();
        let end = start.checked_add(len)?;
        if end > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }
        self.offset = end;
        Some(BdbBlock {
            id: header.id,
            payload_offset: self.base_offset + start,
            data: &self.bytes[start..end],
        })
    }
}

impl<'a> Vbt<'a> {
    /// Parse and validate VBT/BDB headers.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, GmaError> {
        let (header, _) = VbtHeader::ref_from_prefix(bytes).map_err(|_| GmaError::VbtInvalid)?;
        if &header.signature != VBT_SIGNATURE {
            return Err(GmaError::VbtInvalid);
        }
        let header_size = usize::from(header.header_size.get());
        let bdb_offset = usize::from(header.bdb_offset.get());
        let vbt_size = usize::from(header.vbt_size.get());
        if header_size > bytes.len() || vbt_size > bytes.len() {
            return Err(GmaError::VbtInvalid);
        }
        let (bdb, _) = BdbHeader::ref_from_prefix(
            bytes
                .get(bdb_offset..)
                .ok_or(GmaError::VbtInvalid)?,
        )
        .map_err(|_| GmaError::VbtInvalid)?;
        if &bdb.signature != BDB_SIGNATURE {
            return Err(GmaError::VbtInvalid);
        }
        let bdb_header_size = usize::from(bdb.header_size.get());
        let bdb_size = usize::from(bdb.bdb_size.get());
        if bdb_header_size < size_of::<BdbHeader>() || bdb_header_size > bdb_size {
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

    /// Offset of the BDB inside the VBT.
    pub const fn bdb_offset(&self) -> usize {
        self.bdb_offset
    }

    /// Iterate over BDB blocks.
    pub fn blocks(&self) -> BdbBlocks<'a> {
        // Skip the BDB header. Bounds were validated by `parse()`.
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
    let (raw, _) = GeneralDefinitionsRaw::ref_from_prefix(block.data).ok()?;
    Some(GeneralDefinitionsMetadata {
        raw_crt_ddc_gmbus_pin: raw.crt_ddc_gmbus_pin,
        crt_ddc_pin: GmbusPin::from_legacy_select(raw.crt_ddc_gmbus_pin),
        dpms_non_acpi: (raw.dpms & 0x01) != 0,
        skip_boot_crt_detect: (raw.dpms & 0x02) != 0,
        dpms_aim: (raw.dpms & 0x04) != 0,
        boot_display: raw.boot_display,
        child_device_size: raw.child_device_size,
    })
}

fn parse_lfp_options(block: BdbBlock<'_>) -> Result<LvdsPanelOptions, GmaError> {
    let (raw, _) = LvdsOptionsPrefix::ref_from_prefix(block.data)
        .map_err(|_| GmaError::ModeUnavailable)?;
    let raw_panel_type = raw.panel_type;
    let capabilities = raw.capabilities;
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
        lvds_panel_channel_bits: Some(raw.lvds_panel_channel_bits.get()),
        ssc_bits: Some(raw.ssc_bits.get()),
        ssc_freq: Some(raw.ssc_freq.get()),
        panel_color_depth: read_u16_at(block.data, 14),
        dps_panel_type_bits: read_u32_at(block.data, 16),
        backlight_control_type_bits: read_u32_at(block.data, 20),
    })
}

fn parse_lfp_selected_entry(
    ptrs: BdbBlock<'_>,
    data: BdbBlock<'_>,
    panel_type: u8,
) -> Result<(Mode, Option<LfpFpTiming>), GmaError> {
    // BDB 41 starts with a sub-record count, not a panel-slot count. Linux
    // expects the fixed three table groups (fp_timing+dvo_timing+panel_pnp_id)
    // followed by up to 16 panel-slot pointer records. Offsets are
    // BDB-relative and the selected DVO timing table is an EDID DTD.
    let sub_record_count = ptrs
        .data
        .first()
        .copied()
        .ok_or(GmaError::ModeUnavailable)?;
    if sub_record_count != 3 || panel_type > 0x0f {
        return Err(GmaError::ModeUnavailable);
    }

    let table = ptrs.data.get(1..).ok_or(GmaError::ModeUnavailable)?;
    let entry_start = usize::from(panel_type)
        .checked_mul(size_of::<LfpDataPtrEntry>())
        .ok_or(GmaError::ModeUnavailable)?;
    let entry_bytes = table
        .get(entry_start..entry_start + size_of::<LfpDataPtrEntry>())
        .ok_or(GmaError::ModeUnavailable)?;
    let (entry, _) =
        LfpDataPtrEntry::ref_from_prefix(entry_bytes).map_err(|_| GmaError::ModeUnavailable)?;
    let fp_offset = usize::from(entry.fp_timing_offset.get());
    let fp_size = usize::from(entry.fp_timing_size);
    let dvo_offset = usize::from(entry.dvo_timing_offset.get());
    let dvo_size = usize::from(entry.dvo_timing_size);

    if dvo_size < DTD_LEN {
        return Err(GmaError::ModeUnavailable);
    }
    let relative = dvo_offset
        .checked_sub(data.payload_offset)
        .ok_or(GmaError::ModeUnavailable)?;
    let dtd = data
        .data
        .get(relative..relative.checked_add(DTD_LEN).ok_or(GmaError::ModeUnavailable)?)
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
    // still before DVO, so DVO timing bytes are never read as FP registers.
    let available = if gap_to_dvo >= fp_size {
        gap_to_dvo.min(fp_size.saturating_add(6))
    } else {
        gap_to_dvo
    };
    if available < size_of::<LfpFpTimingPrefix>() {
        return None;
    }
    let end = relative.checked_add(available)?;
    let bytes = data.data.get(relative..end)?;
    let (raw, _) = LfpFpTimingPrefix::ref_from_prefix(bytes).ok()?;
    Some(LfpFpTiming {
        x_res: raw.x_res.get(),
        y_res: raw.y_res.get(),
        lvds_reg: raw.lvds_reg.get(),
        lvds_reg_val: raw.lvds_reg_val.get(),
        pp_on_reg: raw.pp_on_reg.get(),
        pp_on_reg_val: raw.pp_on_reg_val.get(),
        pp_off_reg: raw.pp_off_reg.get(),
        pp_off_reg_val: raw.pp_off_reg_val.get(),
        pp_cycle_reg: raw.pp_cycle_reg.get(),
        pp_cycle_reg_val: raw.pp_cycle_reg_val.get(),
        // The panel-fitter registers are only present beyond the prefix.
        pfit_reg: read_u32_at(bytes, 36).unwrap_or(0),
        pfit_reg_val: read_u32_at(bytes, 40).unwrap_or(0),
    })
}

fn parse_lfp_backlight(block: BdbBlock<'_>, panel_type: u8) -> Option<LfpBacklightInfo> {
    let entry_size = usize::from(*block.data.first()?);
    if entry_size < size_of::<LfpBacklightRaw>() {
        return None;
    }
    let start = 1usize.checked_add(usize::from(panel_type).checked_mul(entry_size)?)?;
    let entry = block
        .data
        .get(start..start.checked_add(size_of::<LfpBacklightRaw>())?)?;
    let (raw, _) = LfpBacklightRaw::ref_from_prefix(entry).ok()?;
    Some(LfpBacklightInfo {
        backlight_type: raw.flags & 0x03,
        active_low_pwm: (raw.flags & (1 << 2)) != 0,
        i2c_pin: (raw.flags >> 3) & 0x07,
        i2c_speed: (raw.flags >> 6) & 0x03,
        pwm_freq_hz: raw.pwm_freq_hz.get(),
        min_brightness: raw.min_brightness,
        i2c_address: raw.i2c_address,
        i2c_command: raw.i2c_command,
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

/// Return VBT total size when the header is present.
pub fn declared_vbt_size(bytes: &[u8]) -> Option<u32> {
    let (header, _) = VbtHeader::ref_from_prefix(bytes).ok()?;
    if &header.signature != VBT_SIGNATURE {
        return None;
    }
    Some(u32::from(header.vbt_size.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const X61_VBT: &[u8] = include_bytes!("../../../boards/lenovo/x61/data.vbt");
    const FOXCONN_VBT: &[u8] = include_bytes!("../../../boards/foxconn/d41s/data.vbt");

    fn panel_entry(bytes: &[u8]) -> LfpDataPtrEntry {
        let vbt = Vbt::parse(bytes).unwrap();
        let options = vbt.block(BDB_LVDS_OPTIONS).unwrap();
        let (raw_options, _) = LvdsOptionsPrefix::ref_from_prefix(options.data).unwrap();
        let panel = usize::from(raw_options.panel_type);
        let ptrs = vbt.block(BDB_LVDS_LFP_DATA_PTRS).unwrap();
        let table = &ptrs.data[1..];
        let start = panel * size_of::<LfpDataPtrEntry>();
        let (entry, _) =
            LfpDataPtrEntry::ref_from_prefix(&table[start..start + size_of::<LfpDataPtrEntry>()])
                .unwrap();
        *entry
    }

    fn selected_lfp_offsets(bytes: &[u8]) -> (usize, usize, usize, usize, usize) {
        let vbt = Vbt::parse(bytes).unwrap();
        let options = vbt.block(BDB_LVDS_OPTIONS).unwrap();
        let panel = usize::from(bytes[vbt.bdb_offset + options.payload_offset]);
        let ptrs = vbt.block(BDB_LVDS_LFP_DATA_PTRS).unwrap();
        let data = vbt.block(BDB_LVDS_LFP_DATA).unwrap();
        let record = 1 + panel * 9;
        let entry = panel_entry(bytes);
        let fp_offset = usize::from(entry.fp_timing_offset.get());
        let dvo_offset = usize::from(entry.dvo_timing_offset.get());
        (
            vbt.bdb_offset + ptrs.payload_offset + record,
            vbt.bdb_offset + data.payload_offset + fp_offset - data.payload_offset,
            vbt.bdb_offset + data.payload_offset + dvo_offset - data.payload_offset,
            fp_offset,
            dvo_offset,
        )
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
