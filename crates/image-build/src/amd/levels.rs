// SPDX-License-Identifier: MIT OR Apache-2.0
//! Two-level PSP partitions, as the AMD server and embedded SoCs boot them:
//! the EFS, the `$PSP` directory with the `$PL2` behind its entry 0x40, and
//! the `$BHD` with the `$BL2` behind its entry 0x70. The vendor images of
//! Zeppelin, Naples, Rome and Milan all have this shape; the level-1
//! directories are the recovery set, the PSP and the ABL look a file up in
//! level 2 first.
//!
//! How the directories address their files depends on the boot ROM
//! ([`Addressing`]): Zeppelin and Naples read physical addresses in the
//! flash's mapping below 4 GiB (a first-generation EFS), Rome and Milan
//! flash offsets (a second-generation EFS; Milan says so in the directory
//! headers). An entry the SoC's table puts in both levels is stored once,
//! in level 2's region, and both tables point at it.
//!
//! Signed AMD files are copied unchanged. The reset image (BIOS entry 0x62)
//! lies outside the partition: the caller writes it where the entry says.
use super::{Key, PackedImage, Result, Writer};
use fstart_amdfw_format::{
    directory::{
        BIOS_COOKIE, BIOS_LEVEL2_COOKIE, BiosEntry, PSP_COOKIE, PSP_LEVEL2_COOKIE, PspEntry, bios,
        psp, write_table,
    },
    efs::{BiosDirectorySlot, Efs},
};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes};

/// The directory levels an entry is in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// Only level 1: the recovery set.
    One,
    /// Only level 2.
    Two,
    /// Both levels, stored once.
    #[default]
    Both,
}
impl Level {
    pub const fn in_level1(self) -> bool {
        matches!(self, Self::One | Self::Both)
    }
    pub const fn in_level2(self) -> bool {
        matches!(self, Self::Two | Self::Both)
    }
    /// The SoC table's `level`: 1, 2 or absent for both.
    pub fn from_table(level: Option<u8>) -> Result<Self> {
        match level {
            None => Ok(Self::Both),
            Some(1) => Ok(Self::One),
            Some(2) => Ok(Self::Two),
            Some(other) => Err(format!("directory level {other} is neither 1 nor 2")),
        }
    }
}

/// What an entry describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// A file, copied unchanged.
    File(Vec<u8>),
    /// Erased flash the PSP or the ABL writes (NV data, the APOB NV copy),
    /// 4 KiB aligned.
    Erased(u32),
    /// A value instead of a file (the PSP soft fuse chain, entry 0x0b).
    Value(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: Key,
    pub level: Level,
    pub content: Content,
}

/// How the boot ROM reads the EFS and directory addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// Physical addresses of the flash mapped to end at 4 GiB, a
    /// first-generation EFS (`0xffffffff` at +0x24).
    Physical,
    /// Flash offsets with the directory header's address mode, a
    /// second-generation EFS.
    Offsets { mode: u8 },
}
impl Addressing {
    pub const fn of(slot: BiosDirectorySlot) -> Self {
        match slot {
            BiosDirectorySlot::Family17hModel00 | BiosDirectorySlot::Family17hModel10 => {
                Self::Physical
            }
            BiosDirectorySlot::Family17hModel30 => Self::Offsets { mode: 0 },
            BiosDirectorySlot::Family19h => Self::Offsets { mode: 1 },
        }
    }
    const fn mode(self) -> u32 {
        match self {
            Self::Physical => 0,
            Self::Offsets { mode } => mode as u32,
        }
    }
}

/// The reset image the PSP copies to DRAM and starts x86 in (BIOS entry
/// 0x62, both levels). `size` is the DRAM extent, also of a compressed
/// image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reset {
    pub offset: u32,
    pub size: u32,
    pub destination: u64,
    pub compressed: bool,
}

#[derive(Debug, Clone)]
pub struct TwoLevelImage {
    /// The EFS field the SoC's PSP takes the BIOS directory from.
    pub slot: BiosDirectorySlot,
    /// The flash (or 16 MiB half) the boot ROM maps; offsets below are in it.
    pub flash_size: u32,
    pub efs_offset: u32,
    /// Exclusive end of the partition.
    pub partition_end: u32,
    /// The PSP's SPI read mode and speed, else the erased defaults.
    pub spi: Option<(u8, u8)>,
    pub psp: Vec<Entry>,
    pub bios: Vec<Entry>,
    pub reset: Reset,
    /// The APOB's DRAM destination (BIOS entry 0x61, both levels).
    pub apob_destination: u64,
}

/// Room a directory table takes: the vendor images give each 1 KiB.
fn table_room(entries: usize, entry_size: usize) -> u32 {
    (16 + entries * entry_size)
        .max(0x400)
        .next_multiple_of(0x100) as u32
}

const PSP_ENTRY: usize = size_of::<PspEntry>();
const BIOS_ENTRY: usize = size_of::<BiosEntry>();

/// An entry's place in the partition: the file's offset and size, or the
/// value.
#[derive(Debug, Clone, Copy)]
enum Placed {
    At(u32, u32),
    Value(u64),
}

fn check_entries(entries: &[Entry], bios: bool) -> Result<()> {
    let mut keys = std::collections::BTreeSet::new();
    for entry in entries {
        let Key {
            kind,
            subprogram,
            instance,
        } = entry.key;
        if instance > 15 || (bios && subprogram > 7) {
            return Err(format!("invalid AMD entry selector {:?}", entry.key));
        }
        let reserved = if bios {
            [bios::APOB, bios::RESET_IMAGE, bios::LEVEL2].contains(&kind)
        } else {
            kind == psp::LEVEL2
        };
        if reserved {
            return Err(format!(
                "AMD entry {:?} is the packer's, not a file",
                entry.key
            ));
        }
        match &entry.content {
            Content::File(bytes) if bytes.is_empty() => {
                return Err(format!("empty AMD entry {:?}", entry.key));
            }
            Content::Erased(size) if *size == 0 || size % 0x1000 != 0 => {
                return Err(format!(
                    "erased AMD entry {:?} is not whole 4 KiB blocks",
                    entry.key
                ));
            }
            Content::Value(_) if bios || kind != psp::SOFT_FUSE => {
                return Err(format!("AMD entry {:?} has no value form", entry.key));
            }
            _ => {}
        }
        for level in [1, 2] {
            let present = match level {
                1 => entry.level.in_level1(),
                _ => entry.level.in_level2(),
            };
            if present && !keys.insert((level, entry.key)) {
                return Err(format!("duplicate AMD entry {:?}", entry.key));
            }
        }
    }
    Ok(())
}

/// Place the files of `entries` that `level` stores into `layout`.
fn place(
    layout: &mut Writer,
    entries: &[Entry],
    stored_here: impl Fn(Level) -> bool,
    placed: &mut [Option<Placed>],
) -> Result<()> {
    for (entry, slot) in entries.iter().zip(placed.iter_mut()) {
        if !stored_here(entry.level) {
            continue;
        }
        *slot = Some(match &entry.content {
            Content::File(bytes) => {
                let size = u32::try_from(bytes.len()).map_err(|_| "file too large")?;
                // The APCBs get whole sectors, as the ABL rewrites them.
                let at = if matches!(entry.key.kind, bios::APCB | bios::APCB_BACKUP) {
                    layout.reserve_aligned(size.next_multiple_of(0x1000), 0x1000)?
                } else {
                    layout.reserve_aligned(size, 0x100)?
                };
                layout.write(at, bytes);
                Placed::At(at, size)
            }
            Content::Erased(size) => Placed::At(layout.reserve_aligned(*size, 0x1000)?, *size),
            Content::Value(value) => Placed::Value(*value),
        });
    }
    Ok(())
}

/// The order entries have in a table: by type, the PMU instruction and
/// data files of one selector next to each other.
fn order(key: Key) -> (u8, u8, u8, u8) {
    let kind = if key.kind == bios::PMU_DATA {
        bios::PMU_INSTRUCTIONS
    } else {
        key.kind
    };
    (kind, key.subprogram, key.instance, key.kind)
}

impl TwoLevelImage {
    fn address(&self, offset: u32) -> u64 {
        match Addressing::of(self.slot) {
            Addressing::Physical => (1u64 << 32) - u64::from(self.flash_size) + u64::from(offset),
            Addressing::Offsets { .. } => u64::from(offset),
        }
    }

    /// Directory header word: the region's 4 KiB blocks, SPI block size 1
    /// and the address mode.
    fn additional(&self, start: u32, end: u32) -> Result<u32> {
        let blocks = (end - start).div_ceil(0x1000);
        if blocks > 0x3ff {
            return Err("an AMD directory region exceeds 4 MiB".into());
        }
        Ok(blocks | 1 << 10 | Addressing::of(self.slot).mode() << 29)
    }

    fn check(&self) -> Result<()> {
        if !(0x40000..=0x100_0000).contains(&self.flash_size) || !self.flash_size.is_power_of_two()
        {
            return Err("AMD flash must be a power of two, 256 KiB..16 MiB".into());
        }
        if self.efs_offset >= self.partition_end || self.partition_end > self.flash_size {
            return Err("invalid AMD firmware partition".into());
        }
        if !super::efs_probed(self.flash_size, self.efs_offset) {
            return Err("the EFS is not where the boot ROM searches".into());
        }
        if self.spi.is_some_and(|(mode, speed)| mode > 7 || speed > 5) {
            return Err("invalid EFS SPI read mode or speed".into());
        }
        let reset_end = self
            .reset
            .offset
            .checked_add(self.reset.size)
            .ok_or("reset image overflow")?;
        if self.reset.size == 0
            || reset_end > self.flash_size
            || (self.reset.offset < self.partition_end && reset_end > self.efs_offset)
        {
            return Err("the reset image must lie in the flash, outside the partition".into());
        }
        if self.reset.destination == 0
            || self
                .reset
                .destination
                .checked_add(u64::from(self.reset.size))
                .is_none_or(|end| end > 1 << 32 || end & 0xffff != 0)
        {
            return Err("the reset image must end on a 64-KiB DRAM boundary below 4 GiB".into());
        }
        if self.apob_destination == 0 {
            return Err("missing APOB DRAM destination".into());
        }
        check_entries(&self.psp, false)?;
        check_entries(&self.bios, true)
    }

    /// Write the table at `start` whose region ends at `end`, its entries
    /// in table order.
    fn directory<E: IntoBytes + Immutable>(
        &self,
        layout: &mut Writer,
        start: u32,
        end: u32,
        cookie: [u8; 4],
        mut entries: Vec<(Key, E)>,
    ) -> Result<()> {
        entries.sort_by_key(|(key, _)| order(*key));
        let entries: Vec<E> = entries.into_iter().map(|(_, e)| e).collect();
        let additional = self.additional(start, end)?;
        let at = (start - layout.base) as usize;
        write_table(&mut layout.bytes[at..], cookie, additional, &entries)
            .ok_or("AMD directory outside the partition")?;
        Ok(())
    }

    pub fn pack(&self) -> Result<PackedImage> {
        self.check()?;
        let mut layout = Writer {
            base: self.efs_offset,
            end: self.partition_end,
            bytes: Vec::new(),
        };
        layout.reserve(size_of::<Efs>())?;
        let count = |entries: &[Entry], level: fn(Level) -> bool| {
            entries.iter().filter(|e| level(e.level)).count()
        };
        // The level-1 tables, each followed by its own files, then the
        // level-2 tables with theirs and the shared ones.
        let mut psp = vec![None; self.psp.len()];
        let mut bios = vec![None; self.bios.len()];
        let psp1 = layout.reserve_aligned(
            table_room(count(&self.psp, Level::in_level1) + 1, PSP_ENTRY),
            0x1000,
        )?;
        place(&mut layout, &self.psp, |l| l == Level::One, &mut psp)?;
        let bios1 = layout.reserve_aligned(
            table_room(count(&self.bios, Level::in_level1) + 3, BIOS_ENTRY),
            0x1000,
        )?;
        place(&mut layout, &self.bios, |l| l == Level::One, &mut bios)?;
        let psp2_room = table_room(count(&self.psp, Level::in_level2), PSP_ENTRY);
        let psp2 = layout.reserve_aligned(psp2_room, 0x1000)?;
        place(&mut layout, &self.psp, Level::in_level2, &mut psp)?;
        let bios2_room = table_room(count(&self.bios, Level::in_level2) + 2, BIOS_ENTRY);
        let bios2 = layout.reserve_aligned(bios2_room, 0x1000)?;
        place(&mut layout, &self.bios, Level::in_level2, &mut bios)?;
        layout.align(0x1000)?;
        let end = layout.offset();

        let psp_entry = |entry: &Entry, placed: Placed| match placed {
            Placed::At(at, size) => entry.key.psp_entry(size, self.address(at)),
            Placed::Value(value) => entry.key.psp_entry(u32::MAX, value),
        };
        let bios_entry = |entry: &Entry, placed: Placed| match placed {
            Placed::At(at, size) => entry.key.bios_entry(0, size, self.address(at), u64::MAX),
            Placed::Value(_) => unreachable!("checked: BIOS entries have no values"),
        };
        let level_entries = |level: fn(Level) -> bool| {
            let psp: Vec<_> = self
                .psp
                .iter()
                .zip(&psp)
                .filter(|(e, _)| level(e.level))
                .map(|(e, p)| (e.key, psp_entry(e, p.expect("placed"))))
                .collect();
            let bios: Vec<_> = self
                .bios
                .iter()
                .zip(&bios)
                .filter(|(e, _)| level(e.level))
                .map(|(e, p)| (e.key, bios_entry(e, p.expect("placed"))))
                .collect();
            (psp, bios)
        };
        let fixed = |bios: &mut Vec<(Key, BiosEntry)>| {
            // The APOB has only its destination.
            bios.push((
                Key::new(bios::APOB, 0, 0),
                Key::new(bios::APOB, 0, 0).bios_entry(0, 0, 0, self.apob_destination),
            ));
            let flags = BiosEntry::RESET
                | BiosEntry::COPY
                | if self.reset.compressed {
                    BiosEntry::COMPRESSED
                } else {
                    0
                };
            bios.push((
                Key::new(bios::RESET_IMAGE, 0, 0),
                Key::new(bios::RESET_IMAGE, 0, 0).bios_entry(
                    flags,
                    self.reset.size,
                    self.address(self.reset.offset),
                    self.reset.destination,
                ),
            ));
        };
        let (mut psp_l1, mut bios_l1) = level_entries(Level::in_level1);
        let (psp_l2, mut bios_l2) = level_entries(Level::in_level2);
        fixed(&mut bios_l1);
        fixed(&mut bios_l2);
        psp_l1.push((
            Key::new(psp::LEVEL2, 0, 0),
            Key::new(psp::LEVEL2, 0, 0).psp_entry(psp2_room, self.address(psp2)),
        ));
        bios_l1.push((
            Key::new(bios::LEVEL2, 0, 0),
            Key::new(bios::LEVEL2, 0, 0).bios_entry(0, bios2_room, self.address(bios2), u64::MAX),
        ));
        self.directory(&mut layout, psp1, bios1, PSP_COOKIE, psp_l1)?;
        self.directory(&mut layout, bios1, psp2, BIOS_COOKIE, bios_l1)?;
        self.directory(&mut layout, psp2, bios2, PSP_LEVEL2_COOKIE, psp_l2)?;
        self.directory(&mut layout, bios2, end, BIOS_LEVEL2_COOKIE, bios_l2)?;

        let mut efs = Efs::erased();
        for pointer in [
            &mut efs.imc,
            &mut efs.gec,
            &mut efs.xhci,
            &mut efs.psp_directory,
        ] {
            pointer.set(0);
        }
        efs.new_psp_directory.set(self.address(psp1) as u32);
        if let Addressing::Offsets { .. } = Addressing::of(self.slot) {
            // The second-generation EFS: the generation word the boot ROM
            // checks, and the older BIOS directory fields cleared, as the
            // vendor images have them.
            let profile = fstart_amdfw_format::Profile::for_slot(self.slot);
            efs.generation.set(!profile.generation_mask);
            for slot in [
                BiosDirectorySlot::Family17hModel00,
                BiosDirectorySlot::Family17hModel10,
                BiosDirectorySlot::Family17hModel30,
            ] {
                if slot != self.slot {
                    efs.bios_directory_mut(slot).set(0);
                }
            }
        }
        efs.bios_directory_mut(self.slot)
            .set(self.address(bios1) as u32);
        if let Some((mode, speed)) = self.spi {
            efs.set_spi(self.slot, mode, speed);
        }
        layout.write(self.efs_offset, efs.as_bytes());
        Ok(PackedImage {
            offset: self.efs_offset,
            bytes: layout.bytes,
            psp_directory: psp1,
            bios_directory: bios1,
        })
    }
}

#[cfg(test)]
#[path = "levels_tests.rs"]
mod tests;
