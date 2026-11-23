// SPDX-License-Identifier: MIT OR Apache-2.0
//! ISH A/B dual-origin PSP partitions (Venice / SP7): EFS -> `$PSP` L1 ->
//! 32-byte Image Slot Headers (`0x48` / `0x4A`) -> `$PL2` -> `0x49` -> `$BL2`.
//!
//! All entries inside `$PL2` and `$BL2` use directory-relative addressing
//! (mode 2), while BIOS entry `0x62` (the x86 reset image) uses mode 1
//! relative to `bios_origin` (typically bank 0, offset `0x0`, while the EFS
//! and A/B slots live in bank 1 at `image_origin = 0x0100_0000`). Because
//! every slot-internal reference is directory-relative and `0x62` is relative
//! to `bios_origin`, shared entries deduplicate in metadata while slot-specific
//! entries (`slot: Some(IshSlot::A | IshSlot::B)`) preserve distinct A/B payloads.
//!
//! Signed AMD blobs are copied unchanged. Writable entries (`0x38`, `0x60`,
//! `0x63`, `0x7b`, or any erased region) are 4 KiB-aligned on both sides so
//! SPI NOR sector erases cannot disturb adjacent payloads.

use super::{
    Key, PackedImage, Result, Writer,
    levels::{Content, Reset},
};
use fstart_amdfw_format::{
    directory::{
        BIOS_LEVEL2_COOKIE, BiosEntry, ImageSlotHeader, PSP_COOKIE, PSP_LEVEL2_COOKIE, PspEntry,
        bios, psp, write_table,
    },
    efs::{BiosDirectorySlot, Efs, PROBES, WINDOW},
};
use zerocopy::{Immutable, IntoBytes};

const PSP_ENTRY: usize = size_of::<PspEntry>();
const BIOS_ENTRY: usize = size_of::<BiosEntry>();
const MODE_1: u64 = 1u64 << 62;
const MODE_2: u64 = 2u64 << 62;

/// Target slot within an ISH A/B partition.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum IshSlot {
    A,
    B,
}

impl IshSlot {
    pub const fn psp_kind(self) -> u8 {
        match self {
            Self::A => psp::RECOVERY_A,
            Self::B => psp::RECOVERY_B,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::A => "slot A",
            Self::B => "slot B",
        }
    }

    pub const fn matches(filter: Option<Self>, slot: Self) -> bool {
        match filter {
            None => true,
            Some(s) => matches!((s, slot), (Self::A, Self::A) | (Self::B, Self::B)),
        }
    }
}

/// An entry placed in `$PL2` or `$BL2` of an ISH slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: Key,
    /// Restrict this entry to Slot A (`0x48`) or Slot B (`0x4A`); both if `None`.
    pub slot: Option<IshSlot>,
    /// Set the PSP/BIOS writable attribute bit and 4 KiB-align the allocation.
    pub writable: bool,
    pub content: Content,
}

/// An ISH A/B dual-origin PSP partition.
#[derive(Debug, Clone)]
pub struct IshImage {
    /// EFS generation bits (+0x24) required clear by the boot ROM (e.g. `3`).
    pub generation_mask: u32,
    /// Expected PSP ID written into both 32-byte ISH headers (e.g. `0xbf00_0000`).
    pub psp_id: u32,
    /// Flash offset of the 16 MiB bank holding the EFS and ISH A/B slots.
    pub image_origin: u32,
    /// Flash offset of the 16 MiB bank used as the base for mode-1 BIOS entries.
    pub bios_origin: u32,
    /// Flash offset of the EFS (at a boot-ROM probe offset within `image_origin`).
    pub efs_offset: u32,
    /// Exclusive flash end of the ISH partition within the `image_origin` bank.
    pub partition_end: u32,
    /// eSPI configuration (EFS +0x50), erased if absent.
    pub espi: Option<[u8; 4]>,
    /// From the board donor; leave erased if unspecified.
    pub efs_byte_59: Option<u8>,
    pub psp: Vec<Entry>,
    pub bios: Vec<Entry>,
    pub reset: Reset,
    /// DRAM address where the PSP writes the APOB (BIOS entry `0x61`).
    pub apob_destination: u64,
}

#[derive(Debug, Clone, Copy)]
enum Placed {
    At(u32, u32),
    Value(u64),
}

/// Directory table allocation rounded up to a whole KiB (1..=15 KiB for v1
/// `HEADER_KIB`).
fn table_room_kib(entries: usize, entry_size: usize) -> Result<u32> {
    let raw = 16usize
        .checked_add(
            entries
                .checked_mul(entry_size)
                .ok_or("AMD directory entry count overflow")?,
        )
        .ok_or("AMD directory size overflow")?;
    let kib = raw.div_ceil(1024).max(1);
    if kib > 0xf {
        return Err("AMD v1 directory table exceeds 15 KiB".into());
    }
    Ok((kib * 1024) as u32)
}

/// Directory header v1 (`additional` word) with address mode 2.
fn v1_header(header_bytes: u32, dir_bytes: u32) -> Result<u32> {
    let header_kib = header_bytes / 1024;
    let blocks = dir_bytes / 4096;
    if !header_bytes.is_multiple_of(1024) || !(1..=0xf).contains(&header_kib) {
        return Err("AMD v1 directory header size out of range".into());
    }
    if !dir_bytes.is_multiple_of(4096) || !(1..=0xffff).contains(&blocks) {
        return Err("AMD v1 directory size out of range".into());
    }
    Ok((1u32 << 31) | (2u32 << 24) | (header_kib << 20) | blocks)
}

fn check_entries(entries: &[Entry], is_bios: bool) -> Result<()> {
    let mut keys_a = std::collections::BTreeSet::new();
    let mut keys_b = std::collections::BTreeSet::new();
    for entry in entries {
        let Key {
            kind,
            subprogram,
            instance,
        } = entry.key;
        if subprogram > 7 || instance > 15 {
            return Err(format!("AMD entry {:?} selector out of range", entry.key));
        }
        if IshSlot::matches(entry.slot, IshSlot::A) && !keys_a.insert(entry.key) {
            return Err(format!("duplicate AMD entry {:?} in slot A", entry.key));
        }
        if IshSlot::matches(entry.slot, IshSlot::B) && !keys_b.insert(entry.key) {
            return Err(format!("duplicate AMD entry {:?} in slot B", entry.key));
        }
        if is_bios {
            if matches!(kind, bios::LEVEL2 | bios::APOB | bios::RESET_IMAGE) {
                return Err(format!("reserved BIOS entry {kind:#x}"));
            }
            if matches!(entry.content, Content::Value(_)) {
                return Err(format!("BIOS entry {kind:#x} cannot be a value"));
            }
        } else if matches!(
            kind,
            psp::LEVEL2 | psp::BIOS_TABLE | psp::RECOVERY_A | psp::RECOVERY_B
        ) {
            return Err(format!("reserved PSP entry {kind:#x}"));
        }
        match &entry.content {
            Content::File(bytes) if bytes.is_empty() => {
                return Err(format!("empty AMD firmware blob {:?}", entry.key));
            }
            Content::Erased(0) => {
                return Err(format!("zero-size erased AMD entry {:?}", entry.key));
            }
            _ => {}
        }
    }
    Ok(())
}

fn place_entries(layout: &mut Writer, entries: &[&Entry]) -> Result<Vec<Placed>> {
    let mut placed = Vec::with_capacity(entries.len());
    for entry in entries {
        let p = match &entry.content {
            Content::File(bytes) => {
                let align = if entry.writable { 0x1000 } else { 0x100 };
                let (at, size) = layout.blob(bytes, align)?;
                Placed::At(at, size)
            }
            Content::Erased(size) => {
                let at = layout.reserve_aligned(*size, 0x1000)?;
                layout.align(0x1000)?;
                Placed::At(at, *size)
            }
            Content::Value(value) => Placed::Value(*value),
        };
        placed.push(p);
    }
    Ok(placed)
}

fn write_v1_directory<E: IntoBytes + Immutable>(
    layout: &mut Writer,
    start: u32,
    header_bytes: u32,
    dir_bytes: u32,
    cookie: [u8; 4],
    mut entries: Vec<(Key, E)>,
    sort: bool,
) -> Result<()> {
    if sort {
        entries.sort_by_key(|(key, _)| super::bios_order(*key));
    }
    let entries: Vec<E> = entries.into_iter().map(|(_, e)| e).collect();
    let additional = v1_header(header_bytes, dir_bytes)?;
    let at = (start - layout.base) as usize;
    write_table(&mut layout.bytes[at..], cookie, additional, &entries)
        .ok_or("AMD directory outside the partition")?;
    Ok(())
}

impl IshImage {
    fn check(&self) -> Result<()> {
        if self.image_origin & (WINDOW - 1) != 0 || self.bios_origin & (WINDOW - 1) != 0 {
            return Err("ISH bank origins must be 16 MiB aligned".into());
        }
        if Efs::venice_layout_selector_for(self.image_origin, self.bios_origin).is_none() {
            return Err(format!(
                "unsupported Venice EFS layout origins (image={:#x}, bios={:#x})",
                self.image_origin, self.bios_origin
            ));
        }
        let bank_end = self
            .image_origin
            .checked_add(WINDOW)
            .ok_or("ISH bank overflow")?;
        if self.efs_offset < self.image_origin
            || !PROBES.contains(&(self.efs_offset - self.image_origin))
        {
            return Err(format!(
                "EFS offset {:#x} is not a boot-ROM probe within bank {:#x}",
                self.efs_offset, self.image_origin
            ));
        }
        if self.partition_end <= self.efs_offset
            || self.partition_end > bank_end
            || !self.partition_end.is_multiple_of(0x1000)
        {
            return Err("ISH partition bounds invalid".into());
        }
        let reset_end = self
            .reset
            .offset
            .checked_add(self.reset.size)
            .ok_or("reset image overflow")?;
        if self.reset.size == 0
            || self.reset.offset < self.bios_origin
            || reset_end - self.bios_origin > WINDOW
            || (self.reset.offset < self.partition_end && reset_end > self.efs_offset)
        {
            return Err("reset image outside BIOS bank or overlaps ISH partition".into());
        }
        if self.reset.destination == 0
            || self.apob_destination == 0
            || self
                .reset
                .destination
                .checked_add(u64::from(self.reset.size))
                .is_none_or(|end| end > 0x1_0000_0000 || end & 0xffff != 0)
        {
            return Err("reset image DRAM range or APOB destination invalid".into());
        }
        check_entries(&self.psp, false)?;
        check_entries(&self.bios, true)?;
        Ok(())
    }

    /// Build one self-contained ISH slot (`$PL2` + PSP blobs + `$BL2` + BIOS
    /// blobs) for `which`, returning its bytes and the slot-relative offset of `$BL2`.
    fn build_slot(&self, which: IshSlot, max_size: u32) -> Result<(Vec<u8>, u32)> {
        let psp_entries: Vec<&Entry> = self
            .psp
            .iter()
            .filter(|e| IshSlot::matches(e.slot, which))
            .collect();
        let bios_entries: Vec<&Entry> = self
            .bios
            .iter()
            .filter(|e| IshSlot::matches(e.slot, which))
            .collect();
        let pl2_room = table_room_kib(psp_entries.len() + 1, PSP_ENTRY)?;
        let bl2_room = table_room_kib(bios_entries.len() + 2, BIOS_ENTRY)?;

        let mut slot = Writer {
            base: 0,
            end: max_size,
            bytes: Vec::new(),
        };
        slot.reserve(pl2_room as usize)?;
        let placed_psp = place_entries(&mut slot, &psp_entries)?;
        slot.align(0x1000)?;
        let bl2_rel = slot.offset();
        slot.reserve(bl2_room as usize)?;
        let placed_bios = place_entries(&mut slot, &bios_entries)?;
        slot.align(0x1000)?;
        let slot_end = slot.offset();

        let mut pl2_entries: Vec<(Key, PspEntry)> = psp_entries
            .iter()
            .zip(placed_psp)
            .map(|(entry, placed)| {
                let raw = match placed {
                    Placed::At(at, size) => entry
                        .key
                        .psp_entry(size, MODE_2 | u64::from(at))
                        .with_writable(entry.writable),
                    Placed::Value(value) => entry.key.psp_entry(u32::MAX, value),
                };
                (entry.key, raw)
            })
            .collect();
        pl2_entries.push((
            Key::new(psp::BIOS_TABLE, 0, 0),
            Key::new(psp::BIOS_TABLE, 0, 0).psp_entry(bl2_room, MODE_2 | u64::from(bl2_rel)),
        ));

        let mut bl2_entries: Vec<(Key, BiosEntry)> = bios_entries
            .iter()
            .zip(placed_bios)
            .map(|(entry, placed)| match placed {
                Placed::At(at, size) => (
                    entry.key,
                    entry
                        .key
                        .bios_entry(0, size, MODE_2 | u64::from(at - bl2_rel), u64::MAX)
                        .with_writable(entry.writable),
                ),
                Placed::Value(_) => unreachable!("checked: BIOS entries have no values"),
            })
            .collect();
        bl2_entries.push((
            Key::new(bios::APOB, 0, 0),
            Key::new(bios::APOB, 0, 0).bios_entry(0, 0, 0, self.apob_destination),
        ));
        let reset_flags = BiosEntry::RESET
            | BiosEntry::COPY
            | if self.reset.compressed {
                BiosEntry::COMPRESSED
            } else {
                0
            };
        bl2_entries.push((
            Key::new(bios::RESET_IMAGE, 0, 0),
            Key::new(bios::RESET_IMAGE, 0, 0).bios_entry(
                reset_flags,
                self.reset.size,
                MODE_1 | u64::from(self.reset.offset - self.bios_origin),
                self.reset.destination,
            ),
        ));

        write_v1_directory(
            &mut slot,
            0,
            pl2_room,
            bl2_rel,
            PSP_LEVEL2_COOKIE,
            pl2_entries,
            true,
        )?;
        write_v1_directory(
            &mut slot,
            bl2_rel,
            bl2_room,
            slot_end - bl2_rel,
            BIOS_LEVEL2_COOKIE,
            bl2_entries,
            true,
        )?;

        Ok((slot.bytes, bl2_rel))
    }

    pub fn pack(&self) -> Result<PackedImage> {
        self.check()?;
        let mut layout = Writer {
            base: self.efs_offset,
            end: self.partition_end,
            bytes: Vec::new(),
        };
        layout.reserve(size_of::<Efs>())?;
        // Keep the EFS in its own 64 KiB erase block (matching vendor 0x20000 -> 0x30000).
        let psp1 = layout.reserve_aligned(0x1000, 0x10000)?;
        let psp1_backup = layout.reserve_aligned(0x1000, 0x1000)?;
        let ish_a = layout.reserve_aligned(0x1000, 0x1000)?;
        let ish_b = layout.reserve_aligned(0x1000, 0x1000)?;
        let slot_a = layout.offset();
        let remaining = self
            .partition_end
            .checked_sub(slot_a)
            .ok_or("ISH partition too small for A/B slots")?;
        let slot_max = (remaining / 2) & !0xfff;
        if slot_max < 0x2000 {
            return Err("ISH partition too small for two slots".into());
        }
        let (slot_a_bytes, bl2_rel_a) = self.build_slot(IshSlot::A, slot_max)?;
        let (slot_b_bytes, _bl2_rel_b) = self.build_slot(IshSlot::B, slot_max)?;
        let slot_b = slot_a + slot_max;

        layout.reserve(slot_a_bytes.len())?;
        layout.write(slot_a, &slot_a_bytes);
        let gap = (slot_b - layout.offset()) as usize;
        layout.reserve(gap)?;
        layout.reserve(slot_b_bytes.len())?;
        layout.write(slot_b, &slot_b_bytes);

        let header_a = ImageSlotHeader::new(
            u32::MAX,
            2,
            0,
            slot_a - self.image_origin,
            self.psp_id,
            u32::MAX,
        );
        let header_b =
            ImageSlotHeader::new(2, 2, 0, slot_b - self.image_origin, self.psp_id, u32::MAX);
        layout.write(ish_a, header_a.as_bytes());
        layout.write(ish_b, header_b.as_bytes());

        let l1_entries = vec![
            (
                Key::new(psp::RECOVERY_A, 0, 0),
                Key::new(psp::RECOVERY_A, 0, 0)
                    .psp_entry(0, MODE_1 | u64::from(ish_a - self.image_origin)),
            ),
            (
                Key::new(psp::RECOVERY_B, 0, 0),
                Key::new(psp::RECOVERY_B, 0, 0)
                    .psp_entry(0, MODE_1 | u64::from(ish_b - self.image_origin)),
            ),
        ];
        write_v1_directory(
            &mut layout,
            psp1,
            0x400,
            0x1000,
            PSP_COOKIE,
            l1_entries.clone(),
            false,
        )?;
        write_v1_directory(
            &mut layout,
            psp1_backup,
            0x400,
            0x1000,
            PSP_COOKIE,
            l1_entries,
            false,
        )?;

        let mut efs = Efs::erased();
        for pointer in [
            &mut efs.imc,
            &mut efs.gec,
            &mut efs.xhci,
            &mut efs.psp_directory,
        ] {
            pointer.set(0);
        }
        for slot in [
            BiosDirectorySlot::Family17hModel00,
            BiosDirectorySlot::Family17hModel10,
            BiosDirectorySlot::Family17hModel30,
            BiosDirectorySlot::Family19h,
        ] {
            efs.bios_directory_mut(slot).set(0);
        }
        efs.new_psp_directory.set(psp1 - self.image_origin);
        efs.psp_backup_directory
            .set(psp1_backup - self.image_origin);
        efs.generation.set(!self.generation_mask);
        efs.rom_layout_selector =
            Efs::venice_layout_selector_for(self.image_origin, self.bios_origin)
                .ok_or("unsupported Venice EFS layout origins")?;
        if let Some(value) = self.efs_byte_59 {
            efs.reserved_59[0] = value;
        }
        if let Some([espi0_config0, espi1_config0, espi0_config1, espi1_config1]) = self.espi {
            efs.espi = fstart_amdfw_format::efs::Espi {
                espi0_config0,
                espi1_config0,
                espi0_config1,
                espi1_config1,
            };
        }
        layout.write(self.efs_offset, efs.as_bytes());

        Ok(PackedImage {
            offset: self.efs_offset,
            bytes: layout.bytes,
            psp_directory: psp1,
            bios_directory: slot_a + bl2_rel_a,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_amdfw_format::{Flash, Profile};

    fn sample_ish() -> IshImage {
        IshImage {
            generation_mask: 3,
            psp_id: 0xbf00_0000,
            image_origin: 0x0100_0000,
            bios_origin: 0x0,
            efs_offset: 0x0102_0000,
            partition_end: 0x0200_0000,
            espi: Some([0x0e, 0xff, 0xff, 0xff]),
            efs_byte_59: Some(0x71),
            psp: vec![
                Entry {
                    key: Key::new(0x00, 0, 0),
                    slot: None,
                    writable: false,
                    content: Content::File(vec![0xaa; 0x600]),
                },
                Entry {
                    key: Key::new(0x0b, 0, 0),
                    slot: None,
                    writable: false,
                    content: Content::Value(0x1),
                },
                Entry {
                    key: Key::new(0x38, 0, 0),
                    slot: None,
                    writable: true,
                    content: Content::Erased(0x8000),
                },
                Entry {
                    key: Key::new(0xa0, 0, 2),
                    slot: None,
                    writable: false,
                    content: Content::File(vec![0x55; 0x800]),
                },
            ],
            bios: vec![
                Entry {
                    key: Key::new(0x60, 0, 0),
                    slot: None,
                    writable: true,
                    content: Content::File(vec![0x11; 0x1000]),
                },
                Entry {
                    key: Key::new(0x63, 0, 0),
                    slot: None,
                    writable: true,
                    content: Content::Erased(0x2000),
                },
                Entry {
                    key: Key::new(0x68, 0, 0),
                    slot: None,
                    writable: false,
                    content: Content::File(vec![0x22; 0x800]),
                },
            ],
            reset: Reset {
                offset: 0x00d0_0000,
                size: 0x0030_0000,
                destination: 0x7000_0000,
                compressed: false,
            },
            apob_destination: 0x7700_0000,
        }
    }

    #[test]
    fn packs_venice_ish_ab_and_validates_with_amdfw_format() {
        let img = sample_ish();
        let packed = img.pack().unwrap();
        assert_eq!(packed.offset, 0x0102_0000);
        assert_eq!(packed.psp_directory, 0x0103_0000);

        let mut rom = vec![0xffu8; 64 << 20];
        rom[packed.offset as usize..packed.offset as usize + packed.bytes.len()]
            .copy_from_slice(&packed.bytes);
        rom[0x00d0_0000..0x0100_0000].fill(0x90);

        let flash =
            Flash::region(&rom, 0x0100_0000, 0xff00_0000, Profile::VENICE).with_bios_origin(0x0);
        let (efs_off, efs) = flash.efs_checked().unwrap();
        assert_eq!(efs_off, 0x0102_0000);
        assert_eq!(efs.generation.get(), !3u32);
        assert_eq!(efs.espi.espi0_config0, 0x0e);
        assert_eq!(efs.rom_layout_selector, 1);
        assert_eq!(efs.reserved_59[0], 0x71);
        assert_eq!(efs.new_psp_directory.get(), 0x0003_0000);
        assert_eq!(efs.psp_backup_directory.get(), 0x0003_1000);

        let (slots, count) = flash.ish_slots_checked().unwrap();
        assert_eq!(count, 2);
        let slot_a = slots[0].unwrap();
        let slot_b = slots[1].unwrap();
        assert_eq!(slot_a.kind, psp::RECOVERY_A);
        assert_eq!(slot_b.kind, psp::RECOVERY_B);
        assert_eq!(slot_a.header.priority.get(), u32::MAX);
        assert_eq!(slot_b.header.priority.get(), 2);
        assert_eq!(slot_a.header.psp_id.get(), 0xbf00_0000);
        assert_eq!(slot_b.header.psp_id.get(), 0xbf00_0000);

        for slot in [slot_a, slot_b] {
            let (pl2, bl2) = flash.ish_slot_directories(&slot).unwrap();
            let bl2 = bl2.expect("$BL2 present");
            // Check writable flags and 4 KiB alignment
            let sev = pl2.entries.iter().find(|e| e.kind == 0x38).unwrap();
            assert!(sev.writable());
            assert_eq!(pl2.range(sev).unwrap().start & 0xfff, 0);
            let apcb = bl2.entries.iter().find(|e| e.kind == 0x60).unwrap();
            assert!(apcb.writable());
            assert_eq!(bl2.range(apcb).unwrap().start & 0xfff, 0);
            let apob_nv = bl2.entries.iter().find(|e| e.kind == 0x63).unwrap();
            assert!(apob_nv.writable());
            assert_eq!(bl2.range(apob_nv).unwrap().start & 0xfff, 0);
            let backup = bl2.entries.iter().find(|e| e.kind == 0x68).unwrap();
            assert!(!backup.writable());
            // Check mode-1 dual-origin reset image resolves to Bank 0
            let reset = bl2
                .entries
                .iter()
                .find(|e| e.kind == bios::RESET_IMAGE)
                .unwrap();
            assert_eq!(bl2.range(reset).unwrap(), 0x00d0_0000..0x0100_0000);
            assert_eq!(reset.destination(), 0x7000_0000);
            let apob = bl2.entries.iter().find(|e| e.kind == bios::APOB).unwrap();
            assert_eq!(apob.destination(), 0x7700_0000);
        }

        flash
            .validate_prefix(BiosDirectorySlot::Family19h, rom.len(), None)
            .unwrap();
    }

    #[test]
    fn rejects_overlapping_reset_and_bad_probe() {
        let mut bad = sample_ish();
        bad.reset.offset = 0x0102_0000;
        assert!(bad.pack().is_err());

        let mut bad_efs = sample_ish();
        bad_efs.efs_offset = 0x0101_0000;
        assert!(bad_efs.pack().is_err());
    }

    #[test]
    fn donor_extract_and_has_firmware_round_trip_ish_image() {
        use crate::amd::blobs::{
            BoardApcb, BoardBlobs, DirectoryFile, MicrocodeFile, SocBlobs, donor_image, extract,
            has_firmware,
        };
        let mut img = sample_ish();
        // Add a Venice A0 (0xb500) microcode patch in $BL2 to verify extraction.
        let mut ucode = vec![0u8; 0x3bc0];
        ucode[4..8].copy_from_slice(&0x0b50_0023u32.to_le_bytes());
        ucode[0x18..0x1a].copy_from_slice(&0xb500u16.to_le_bytes());
        img.bios.push(Entry {
            key: Key::new(bios::MICROCODE, 0, 0),
            writable: false,
            slot: None,
            content: Content::File(ucode.clone()),
        });
        let packed = img.pack().unwrap();
        let mut rom = vec![0xffu8; 64 << 20];
        rom[packed.offset as usize..packed.offset as usize + packed.bytes.len()]
            .copy_from_slice(&packed.bytes);

        let soc = SocBlobs {
            bios_directory: BiosDirectorySlot::Family19h,
            levels: 2,
            generation_mask: Some(3),
            ish: true,
            image_origin: 0x0100_0000,
            bios_origin: Some(0x0),
            psp_id: Some(0xbf00_0000),
            efs_offset: 0x0102_0000,
            partition_end: 0x0200_0000,
            spi_read_mode: None,
            spi_speed: None,
            micron: false,
            soft_fuse: None,
            apob_destination: None,
            psp: vec![
                DirectoryFile {
                    kind: 0x00,
                    file: Some("venice/pub.bin".into()),
                    ..Default::default()
                },
                DirectoryFile {
                    kind: 0x0b,
                    value: Some(1),
                    ..Default::default()
                },
                DirectoryFile {
                    kind: 0x38,
                    erased: Some(0x8000),
                    slot: None,
                    writable: true,
                    ..Default::default()
                },
                DirectoryFile {
                    kind: 0xa0,
                    instance: 2,
                    file: Some("venice/a0_2.bin".into()),
                    ..Default::default()
                },
            ],
            bios: vec![DirectoryFile {
                kind: 0x63,
                erased: Some(0x2000),
                slot: None,
                writable: true,
                ..Default::default()
            }],
            microcode: vec![MicrocodeFile {
                equivalence_id: 0xb500,
                file: "venice/ucode/b500.bin".into(),
            }],
            vbios: vec![],
        };
        let board = BoardBlobs {
            soc: "venice".into(),
            psp_app: false,
            apcb: BoardApcb::Entries {
                entries: vec![
                    DirectoryFile {
                        kind: 0x60,
                        slot: None,
                        writable: true,
                        file: Some("board/apcb-60.bin".into()),
                        ..Default::default()
                    },
                    DirectoryFile {
                        kind: 0x68,
                        file: Some("board/apcb-68.bin".into()),
                        ..Default::default()
                    },
                ],
            },
            apob_destination: None,
            efs_espi: Some([0x0e, 0xff, 0xff, 0xff]),
            efs_byte_59: Some(0x71),
        };

        assert!(donor_image(&rom, &soc).is_some());
        assert!(has_firmware(&[(soc.clone(), board.clone())], &rom));

        let (found, missing) = extract(&soc, &board, "board", &rom).unwrap();
        assert!(missing.is_empty(), "missing: {missing:?}");
        let get = |name: &str| found.iter().find(|f| f.file == name).unwrap();
        assert_eq!(get("venice/pub.bin").bytes, [0xaa; 0x600]);
        assert_eq!(get("venice/a0_2.bin").bytes, [0x55; 0x800]);
        assert_eq!(get("board/apcb-60.bin").bytes, [0x11; 0x1000]);
        assert_eq!(get("board/apcb-68.bin").bytes, [0x22; 0x800]);
        assert_eq!(get("venice/ucode/b500.bin").bytes, ucode);
    }

    #[test]
    fn preserves_distinct_slot_a_and_b_payloads_and_psp_0x66_namespace() {
        use super::super::blobs::{
            BoardApcb, BoardBlobs, DirectoryFile, SocBlobs, extract, resolve,
        };

        let mut img = sample_ish();
        // Replace shared 0x00 with distinct slot A and slot B payloads, and add PSP type 0x66.
        img.psp.retain(|e| e.key.kind != 0x00);
        img.psp.push(Entry {
            key: Key::new(0x00, 0, 0),
            slot: Some(IshSlot::A),
            writable: false,
            content: Content::File(vec![0x1a; 0x600]),
        });
        img.psp.push(Entry {
            key: Key::new(0x00, 0, 0),
            slot: Some(IshSlot::B),
            writable: false,
            content: Content::File(vec![0x2b; 0x600]),
        });
        img.psp.push(Entry {
            key: Key::new(0x66, 0, 0),
            slot: None,
            writable: false,
            content: Content::File(vec![0x66; 0x200]),
        });

        let packed = img.pack().unwrap();
        let mut rom = vec![0xffu8; 64 << 20];
        rom[packed.offset as usize..packed.offset as usize + packed.bytes.len()]
            .copy_from_slice(&packed.bytes);
        rom[0x00d0_0000..0x0100_0000].fill(0x90);

        // EFS +0x58 encodes selector 1; validate without out-of-band bios_origin.
        assert_eq!(rom[0x0102_0000 + 0x58], 1);
        let flash = Flash::region(&rom, 0x0100_0000, 0xff00_0000, Profile::VENICE);
        assert_eq!(flash.checked_bios_origin(), Ok(0));
        flash
            .validate_prefix(Profile::VENICE.bios, rom.len(), None)
            .unwrap();

        let (slots, count) = flash.ish_slots_checked().unwrap();
        assert_eq!(count, 2);
        let (pl2_a, _) = flash.ish_slot_directories(&slots[0].unwrap()).unwrap();
        let (pl2_b, _) = flash.ish_slot_directories(&slots[1].unwrap()).unwrap();
        let p0_a = pl2_a.entries.iter().find(|e| e.kind == 0x00).unwrap();
        let p0_b = pl2_b.entries.iter().find(|e| e.kind == 0x00).unwrap();
        assert_eq!(pl2_a.data(p0_a).unwrap(), &[0x1a; 0x600]);
        assert_eq!(pl2_b.data(p0_b).unwrap(), &[0x2b; 0x600]);
        // PSP type 0x66 is distinct from BIOS microcode 0x66 and survives packing.
        let p66_a = pl2_a.entries.iter().find(|e| e.kind == 0x66).unwrap();
        assert_eq!(pl2_a.data(p66_a).unwrap(), &[0x66; 0x200]);

        // Extracting a differing entry as shared (slot = None) fails with conflict.
        let shared_soc = SocBlobs {
            bios_directory: BiosDirectorySlot::Family19h,
            levels: 2,
            generation_mask: Some(3),
            ish: true,
            image_origin: 0x0100_0000,
            bios_origin: Some(0),
            psp_id: Some(0xbf00_0000),
            efs_offset: 0x0102_0000,
            partition_end: 0x0200_0000,
            spi_read_mode: None,
            spi_speed: None,
            micron: false,
            soft_fuse: None,
            apob_destination: Some(0x7700_0000),
            psp: vec![DirectoryFile {
                kind: 0x00,
                file: Some("venice/pub.bin".into()),
                ..Default::default()
            }],
            bios: vec![],
            microcode: vec![],
            vbios: vec![],
        };
        let board = BoardBlobs {
            soc: "venice".into(),
            psp_app: false,
            apcb: BoardApcb::Entries { entries: vec![] },
            apob_destination: None,
            efs_espi: None,
            efs_byte_59: None,
        };
        assert!(extract(&shared_soc, &board, "board", &rom).is_err());

        // Declaring slot = "a" and slot = "b" extracts both distinct payloads cleanly.
        let mut split_soc = shared_soc.clone();
        split_soc.psp = vec![
            DirectoryFile {
                kind: 0x00,
                slot: Some(IshSlot::A),
                file: Some("venice/pub_a.bin".into()),
                ..Default::default()
            },
            DirectoryFile {
                kind: 0x00,
                slot: Some(IshSlot::B),
                file: Some("venice/pub_b.bin".into()),
                ..Default::default()
            },
            DirectoryFile {
                kind: 0x66,
                file: Some("venice/psp_66.bin".into()),
                ..Default::default()
            },
        ];
        let (found, missing) = extract(&split_soc, &board, "board", &rom).unwrap();
        assert!(missing.is_empty());
        assert_eq!(
            found
                .iter()
                .find(|f| f.file == "venice/pub_a.bin")
                .unwrap()
                .bytes,
            [0x1a; 0x600]
        );
        assert_eq!(
            found
                .iter()
                .find(|f| f.file == "venice/pub_b.bin")
                .unwrap()
                .bytes,
            [0x2b; 0x600]
        );
        assert_eq!(
            found
                .iter()
                .find(|f| f.file == "venice/psp_66.bin")
                .unwrap()
                .bytes,
            [0x66; 0x200]
        );

        // Contradictory ISH metadata (missing bios_origin, levels != 2, or entry level) is rejected.
        let mut bad_soc = split_soc.clone();
        bad_soc.bios_origin = None;
        assert!(
            resolve(
                &bad_soc,
                &board,
                "board",
                std::path::Path::new("blobs"),
                64 << 20
            )
            .is_err()
        );
        let mut bad_levels = split_soc.clone();
        bad_levels.levels = 1;
        assert!(
            resolve(
                &bad_levels,
                &board,
                "board",
                std::path::Path::new("blobs"),
                64 << 20
            )
            .is_err()
        );
        let mut bad_entry_lvl = split_soc;
        bad_entry_lvl.psp[0].level = Some(2);
        assert!(
            resolve(
                &bad_entry_lvl,
                &board,
                "board",
                std::path::Path::new("blobs"),
                64 << 20
            )
            .is_err()
        );
    }
}
