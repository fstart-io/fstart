// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD firmware image inspection and validation, independent of silicon drivers
//! and packing policy. Based on AMD #55758 and the SP3 reverse engineering;
//! the reader of the firmware (SP3) and of the host tools alike.
//!
//! A flash view has an explicit image origin, physical mapping and optional
//! recovery-slot origin. Directory address modes are never inferred from an
//! address's magnitude except for the documented legacy offset convention.
#![no_std]

pub mod anchor;
pub mod directory;
pub mod efs;
pub mod microcode;

use core::ops::Range;
use directory::{BiosEntry, Directory, Entry, Levels, PspEntry};
use efs::{BiosDirectorySlot, Efs};
use zerocopy::FromBytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoEfs,
    MissingDirectory,
    Truncated(usize),
    Cookie(usize),
    Checksum(usize),
    Address(u64),
    AddressMode(u8),
    DuplicateLevel2,
    NestedLevel2,
    ComboSelection,
    UnsupportedTopology(u8),
    OutsidePrefix { start: usize, end: usize },
    AncillaryPointer(u32),
    ResetFlags,
    DuplicateIshSlot(u8),
    PspIdMismatch { expected: u32, actual: u32 },
}

/// Format policy, not board wiring or a bag of CPU-family conditionals.
/// New platforms can supply their own profile without changing the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub bios: BiosDirectorySlot,
    /// Bits the boot ROM requires clear in EFS +0x24; zero for Gen1.
    pub generation_mask: u32,
    /// Explicit ID for `2PSP` combo selection. No guessing the first entry.
    pub psp_id: Option<u32>,
    /// Family ID used only by combo entries whose id_select is 1.
    pub chip_id: Option<u32>,
    /// Some vendor Gen1/Gen2 mode-0 directories contain image offsets.
    pub legacy_offsets: bool,
    /// Picasso's APOB-NV uses the fixed 16-MiB mapping even on smaller parts.
    pub apob_nv_alias: bool,
    /// Platform uses PSP L1 recovery entries (`0x48`/`0x4A`) pointing to
    /// 32-byte Image Slot Headers (`ImageSlotHeader`) -> `$PL2` -> `0x49` -> `$BL2`.
    pub ish: bool,
}
impl Profile {
    /// Siena's (SP6) boot ROM: the Family 19h BIOS directory slot, but only
    /// bit 0 of the EFS generation word must be clear; its vendor images
    /// keep bit 1 set (FFFFFFFEh), which Milan's mask rejects. No combo
    /// identity: combo images must name theirs explicitly.
    pub const SIENA: Self = Self {
        bios: BiosDirectorySlot::Family19h,
        generation_mask: 1,
        psp_id: None,
        chip_id: None,
        legacy_offsets: true,
        apob_nv_alias: false,
        ish: false,
    };

    /// Genoa's (SP5) boot ROM: the Family 19h BIOS directory slot, only
    /// bit 0 of the EFS generation word clear (AMD's tools write
    /// FFFFFFFEh), and no Milan combo identity.
    pub const GENOA: Self = Self {
        bios: BiosDirectorySlot::Family19h,
        generation_mask: 1,
        psp_id: None,
        chip_id: None,
        legacy_offsets: true,
        apob_nv_alias: false,
        ish: false,
    };

    /// Venice's (SP7) boot ROM: EFS in a 16-MiB bank with `$PSP` L1
    /// pointing to 32-byte ISH slots (`0x48`/`0x4A`, PSP ID `0xBF00_0000`)
    /// -> `$PL2` -> type `0x49` -> `$BL2`, and dual-origin mode-1 BIOS
    /// entries relative to BIOS origin 0.
    pub const VENICE: Self = Self {
        bios: BiosDirectorySlot::Family19h,
        generation_mask: 3,
        psp_id: Some(0xbf00_0000),
        chip_id: None,
        legacy_offsets: true,
        apob_nv_alias: false,
        ish: true,
    };

    pub const fn for_slot(bios: BiosDirectorySlot) -> Self {
        let (generation_mask, psp_id) = match bios {
            BiosDirectorySlot::Family17hModel00 => (0, Some(0xbc09_0000)),
            BiosDirectorySlot::Family17hModel10 => (0, Some(0xbc0a_0000)),
            BiosDirectorySlot::Family17hModel30 => (1, Some(0xbc0b_0000)),
            BiosDirectorySlot::Family19h => (3, Some(0xbc0b_0d00)),
        };
        Self {
            bios,
            generation_mask,
            psp_id,
            chip_id: None,
            legacy_offsets: true,
            apob_nv_alias: matches!(bios, BiosDirectorySlot::Family17hModel10),
            ish: false,
        }
    }
}

/// A view of a whole image. `origin` selects a firmware region in a larger
/// ROM; addresses relative to the image start at that origin, not at byte 0.
#[derive(Clone, Copy)]
pub struct Flash<'a> {
    bytes: &'a [u8],
    pub origin: usize,
    /// Optional separate origin for mode-1 BIOS directory entries (e.g.
    /// Venice dual-origin images where EFS/PSP live at bank `0x0100_0000`
    /// while BIOS `0x62` mode-1 offsets are relative to flash offset 0).
    pub bios_origin: Option<usize>,
    pub physical_base: u64,
    pub slot_origin: Option<usize>,
    pub profile: Profile,
}
impl<'a> Flash<'a> {
    /// Legacy top-mapped flash, retained for the existing Gen1 callers.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            origin: 0,
            bios_origin: None,
            physical_base: (1u64 << 32).saturating_sub(bytes.len() as u64),
            slot_origin: None,
            profile: Profile::for_slot(BiosDirectorySlot::Family17hModel00),
        }
    }
    /// Explicit mapping for SP3, multi-image ROMs and recovery partitions.
    pub const fn region(
        bytes: &'a [u8],
        origin: usize,
        physical_base: u64,
        profile: Profile,
    ) -> Self {
        Self {
            bytes,
            origin,
            bios_origin: None,
            physical_base,
            slot_origin: None,
            profile,
        }
    }
    pub const fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }
    pub const fn with_slot(mut self, origin: usize) -> Self {
        self.slot_origin = Some(origin);
        self
    }
    /// Override the origin used when resolving BIOS directory (`$BL2`)
    /// mode-1 entries on dual-origin ISH images.
    pub const fn with_bios_origin(mut self, origin: usize) -> Self {
        self.bios_origin = Some(origin);
        self
    }
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn address(&self, offset: u32) -> u64 {
        self.physical_base + u64::from(offset)
    }
    pub fn offset(&self, address: u64) -> Option<usize> {
        self.resolve(address, 0, 0).ok()
    }
    pub fn get(&self, address: u64, size: u32) -> Option<&'a [u8]> {
        let start = self.offset(address)?;
        self.bytes.get(start..start.checked_add(size as usize)?)
    }
    /// Resolve the effective directory/entry mode (AMD #55758): the higher
    /// of the entry override and directory mode. Value entries are not addresses.
    pub fn resolve(&self, encoded: u64, directory_mode: u8, table: usize) -> Result<usize, Error> {
        self.resolve_with_mode1_origin(encoded, directory_mode, table, self.origin)
    }
    pub(crate) fn resolve_with_mode1_origin(
        &self,
        encoded: u64,
        directory_mode: u8,
        table: usize,
        mode1_origin: usize,
    ) -> Result<usize, Error> {
        let entry_mode = (encoded >> 62) as u8;
        let mode = if entry_mode != 0 {
            entry_mode
        } else {
            directory_mode
        };
        let address = encoded & !(3u64 << 62);
        let raw = usize::try_from(address).map_err(|_| Error::Address(encoded))?;
        let offset = match mode {
            0 => {
                if (self.physical_base..1u64 << 32).contains(&address) {
                    self.origin
                        .checked_add((address - self.physical_base) as usize)
                } else if self.profile.legacy_offsets {
                    self.origin.checked_add(raw)
                } else {
                    None
                }
            }
            1 => mode1_origin.checked_add(raw),
            2 => table.checked_add(raw),
            3 => self
                .slot_origin
                .ok_or(Error::AddressMode(mode))?
                .checked_add(raw),
            _ => return Err(Error::AddressMode(mode)),
        }
        .filter(|&o| o < self.bytes.len())
        .ok_or(Error::Address(encoded))?;
        Ok(offset)
    }
    /// First EFS accepted by this profile, in boot-ROM probe order. The
    /// caller selects regions explicitly; this never silently takes another
    /// generation's EFS from the end of a multi-image ROM.
    pub fn efs_checked(&self) -> Result<(usize, &'a Efs), Error> {
        efs::PROBES
            .iter()
            .find_map(|&probe| {
                let available = self
                    .bytes
                    .len()
                    .checked_sub(self.origin)?
                    .min(efs::WINDOW as usize);
                let local = available
                    .checked_add(probe as usize)?
                    .checked_sub(efs::WINDOW as usize)?;
                let offset = self.origin.checked_add(local)?;
                let (efs, _) = Efs::ref_from_prefix(self.bytes.get(offset..)?).ok()?;
                (efs.signature.get() == efs::SIGNATURE
                    && efs.generation.get() & self.profile.generation_mask == 0)
                    .then_some((offset, efs))
            })
            .ok_or(Error::NoEfs)
    }
    pub fn efs(&self) -> Option<(usize, &'a Efs)> {
        self.efs_checked().ok()
    }
    pub fn bios_directories_checked(
        &self,
        slot: BiosDirectorySlot,
    ) -> Result<Levels<'a, BiosEntry>, Error> {
        let flash = self.with_profile(Profile {
            bios: slot,
            ..self.profile
        });
        let (_, efs) = flash.efs_checked()?;
        let pointer = efs.bios_directory(slot).ok_or(Error::MissingDirectory)?;
        Levels::read_checked(flash, flash.resolve(pointer, 0, 0)?)
    }
    pub fn bios_directories(&self, slot: BiosDirectorySlot) -> Option<Levels<'a, BiosEntry>> {
        self.bios_directories_checked(slot).ok()
    }
    pub fn bios_files(
        &self,
        slot: BiosDirectorySlot,
        kind: u8,
    ) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        self.bios_directories(slot)
            .into_iter()
            .flat_map(move |levels| levels.all(kind))
            .filter(|(_, entry)| entry.has_data())
            .filter_map(|(dir, entry)| dir.data(entry).ok())
    }
    fn psp_root_levels(&self, pointer: u32) -> Result<Levels<'a, PspEntry>, Error> {
        if matches!(pointer, 0 | u32::MAX) {
            return Err(Error::MissingDirectory);
        }
        let offset = self.resolve(pointer.into(), 0, 0)?;
        match directory::Combo::read(*self, offset)? {
            Some(combo) => {
                // Offline selection is by explicit identity, including on
                // root-key lookup-mode images. It is not key authentication.
                let mut matches = combo.entries().filter(|e| match e.id_select {
                    0 => self
                        .profile
                        .psp_id
                        .is_some_and(|id| e.id & 0xffff_ff00 == id & 0xffff_ff00),
                    1 => self.profile.chip_id == Some(e.id),
                    _ => false,
                });
                let entry = matches.next().ok_or(Error::ComboSelection)?;
                if matches.next().is_some() {
                    return Err(Error::ComboSelection);
                }
                Levels::read_checked(*self, self.resolve(entry.directory, 0, offset)?)
            }
            None => Levels::read_checked(*self, offset),
        }
    }
    pub fn psp_directories(&self) -> Result<Levels<'a, PspEntry>, Error> {
        let (_, efs) = self.efs_checked()?;
        let pointer = [efs.new_psp_directory.get(), efs.psp_directory.get()]
            .into_iter()
            .find(|&p| !matches!(p, 0 | u32::MAX))
            .ok_or(Error::MissingDirectory)?;
        self.psp_root_levels(pointer)
    }
    fn resolve_ish_entry(
        &self,
        dir: Directory<'a, PspEntry>,
        entry: &PspEntry,
    ) -> Result<ResolvedIsh<'a>, Error> {
        if !self.profile.ish {
            return Err(Error::UnsupportedTopology(entry.kind));
        }
        let ish_off = self.resolve(entry.source(), dir.header.address_mode(), dir.offset)?;
        let ish = *directory::ImageSlotHeader::read_checked(*self, ish_off)?;
        if let Some(expected) = self.profile.psp_id
            && ish.psp_id.get() != expected
        {
            return Err(Error::PspIdMismatch {
                expected,
                actual: ish.psp_id.get(),
            });
        }
        let psp_l2_offset = self.resolve(u64::from(ish.location.get()), 1, dir.offset)?;
        let psp_l2 = Directory::<PspEntry>::read_checked(*self, psp_l2_offset, 1)?;
        let mut bios_entries = psp_l2
            .entries
            .iter()
            .filter(|e| e.kind == directory::psp::BIOS_TABLE && e.has_data());
        let bios = match bios_entries.next() {
            Some(bios_entry) => {
                if bios_entries.next().is_some() {
                    return Err(Error::DuplicateLevel2);
                }
                let range = psp_l2.range(bios_entry)?;
                let level = usize::from(
                    self.bytes[range.start..].starts_with(&directory::BIOS_LEVEL2_COOKIE),
                );
                let bios_dir = Directory::<BiosEntry>::read_checked(*self, range.start, level)?;
                if bios_dir.table().len() > range.len() {
                    return Err(Error::Truncated(range.start));
                }
                if level == 1
                    && bios_dir
                        .entries
                        .iter()
                        .any(|e| e.kind == directory::bios::LEVEL2)
                {
                    return Err(Error::NestedLevel2);
                }
                let nested_l2 = bios_dir.level2_checked()?;
                Some(ResolvedIshBios {
                    range,
                    dir: bios_dir,
                    nested_l2,
                })
            }
            None => None,
        };
        let slot = IshSlot {
            kind: entry.kind,
            ish_offset: ish_off,
            header: ish,
            psp_l2_offset,
            bios_l2_offset: bios.as_ref().map(|b| b.range.start),
        };
        Ok(ResolvedIsh { slot, psp_l2, bios })
    }
    /// Enumerate and verify all ISH A/B slots (`0x48` / `0x4A`) reachable
    /// from the selected EFS's PSP L1 directories (primary, legacy, backup,
    /// and combo), resolving each slot's `$PL2` and optional `$BL2` (`0x49`).
    pub fn ish_slots_checked(&self) -> Result<([Option<IshSlot>; 2], usize), Error> {
        let (_, efs) = self.efs_checked()?;
        let pointers = [
            efs.new_psp_directory.get(),
            efs.psp_directory.get(),
            efs.psp_backup_directory.get(),
        ];
        if pointers.iter().all(|&p| matches!(p, 0 | u32::MAX)) {
            return Err(Error::MissingDirectory);
        }
        let mut out: [Option<IshSlot>; 2] = [None; 2];
        let mut count = 0usize;
        let mut seen_roots = [None; 3];
        let mut root_count = 0usize;
        for pointer in pointers {
            if matches!(pointer, 0 | u32::MAX) {
                continue;
            }
            let root_off = self.resolve(u64::from(pointer), 0, 0)?;
            if seen_roots[..root_count].contains(&Some(root_off)) {
                continue;
            }
            seen_roots[root_count] = Some(root_off);
            root_count += 1;
            let levels = self.psp_root_levels(pointer)?;
            for dir in levels.iter() {
                let mut seen_in_dir = [false; 2];
                for entry in dir.entries {
                    let kind_idx = match entry.kind {
                        directory::psp::RECOVERY_A => 0,
                        directory::psp::RECOVERY_B => 1,
                        _ => continue,
                    };
                    if seen_in_dir[kind_idx] {
                        return Err(Error::DuplicateIshSlot(entry.kind));
                    }
                    seen_in_dir[kind_idx] = true;
                    let resolved = self.resolve_ish_entry(dir, entry)?;
                    if let Some(existing) = out[..count]
                        .iter()
                        .flatten()
                        .find(|s| s.kind == resolved.slot.kind)
                    {
                        if existing.ish_offset == resolved.slot.ish_offset {
                            continue;
                        }
                        return Err(Error::DuplicateIshSlot(entry.kind));
                    }
                    if count >= out.len() {
                        return Err(Error::DuplicateIshSlot(entry.kind));
                    }
                    out[count] = Some(resolved.slot);
                    count += 1;
                }
            }
        }
        Ok((out, count))
    }

    /// Check every reachable PSP and BIOS table/file before preserving only
    /// `kept` bytes. The replacement reset slot is the sole permitted exception.
    /// EFS ancillary blobs (IMC, GEC, xHCI, Promontory) have no length; one is
    /// kept when it starts below the lowest directory, which bounds it, and
    /// refused otherwise.
    pub fn validate_prefix(
        &self,
        slot: BiosDirectorySlot,
        kept: usize,
        reset: Option<Range<usize>>,
    ) -> Result<(), Error> {
        if kept > self.bytes.len() {
            return Err(Error::Truncated(kept));
        }
        let (offset, efs) = self.efs_checked()?;
        self.within(offset..offset + size_of::<Efs>(), kept)?;
        let lowest_directory = [
            efs.psp_directory,
            efs.new_psp_directory,
            efs.psp_backup_directory,
        ]
        .into_iter()
        .map(|p| p.get())
        .filter(|&p| !matches!(p, 0 | u32::MAX))
        .map(u64::from)
        .chain(
            [
                BiosDirectorySlot::Family17hModel00,
                BiosDirectorySlot::Family17hModel10,
                BiosDirectorySlot::Family17hModel30,
                BiosDirectorySlot::Family19h,
            ]
            .into_iter()
            .filter_map(|slot| efs.bios_directory(slot)),
        )
        .map(|pointer| self.resolve(pointer, 0, 0))
        .try_fold(usize::MAX, |lowest, offset| {
            Ok::<_, Error>(lowest.min(offset?))
        })?;
        for pointer in [
            efs.imc,
            efs.gec,
            efs.xhci,
            efs.promontory,
            efs.lp_promontory,
            efs.promontory19,
        ] {
            let pointer = pointer.get();
            if !matches!(pointer, 0 | u32::MAX) {
                let blob = self.resolve(pointer.into(), 0, 0)?;
                if kept != self.bytes.len() && blob >= lowest_directory {
                    return Err(Error::AncillaryPointer(pointer));
                }
            }
        }
        if !self.profile.ish {
            let bios = self.bios_directories_checked(slot)?;
            for dir in bios.iter() {
                self.validate_bios_directory(dir, kept, reset.as_ref())?;
            }
        } else if let Some(pointer) = efs.bios_directory(slot) {
            let flash = self.with_profile(Profile {
                bios: slot,
                ..self.profile
            });
            for dir in
                Levels::<BiosEntry>::read_checked(flash, flash.resolve(pointer, 0, 0)?)?.iter()
            {
                self.validate_bios_directory(dir, kept, reset.as_ref())?;
            }
        }
        // Other populated BIOS slots remain reachable from the retained EFS.
        // Use this EFS, not another generation's first accepted probe.
        for other in [
            BiosDirectorySlot::Family17hModel00,
            BiosDirectorySlot::Family17hModel10,
            BiosDirectorySlot::Family17hModel30,
            BiosDirectorySlot::Family19h,
        ] {
            if other == slot {
                continue;
            }
            if let Some(pointer) = efs.bios_directory(other) {
                let flash = self.with_profile(Profile {
                    bios: other,
                    apob_nv_alias: Profile::for_slot(other).apob_nv_alias,
                    ..self.profile
                });
                for dir in
                    Levels::<BiosEntry>::read_checked(flash, flash.resolve(pointer, 0, 0)?)?.iter()
                {
                    self.validate_bios_directory(dir, kept, reset.as_ref())?;
                }
            }
        }
        // Both PSP pointers, plus every combo branch, remain reachable after
        // overlay. Validate all of them, not just the selected CPU's branch.
        let pointers = [
            efs.psp_directory,
            efs.new_psp_directory,
            efs.psp_backup_directory,
        ];
        if pointers.iter().all(|p| matches!(p.get(), 0 | u32::MAX)) {
            return Err(Error::MissingDirectory);
        }
        let mut found_ish_bios = false;
        for pointer in pointers {
            let pointer = pointer.get();
            if matches!(pointer, 0 | u32::MAX) {
                continue;
            }
            let offset = self.resolve(pointer.into(), 0, 0)?;
            match directory::Combo::read(*self, offset)? {
                Some(combo) => {
                    self.within(combo.extent(), kept)?;
                    for entry in combo.entries() {
                        found_ish_bios |= self.validate_psp(
                            self.resolve(entry.directory, 0, offset)?,
                            kept,
                            reset.as_ref(),
                        )?;
                    }
                }
                None => found_ish_bios |= self.validate_psp(offset, kept, reset.as_ref())?,
            }
        }
        if self.profile.ish && !found_ish_bios {
            return Err(Error::MissingDirectory);
        }
        Ok(())
    }
    fn validate_bios_directory(
        &self,
        dir: Directory<'a, BiosEntry>,
        kept: usize,
        reset: Option<&Range<usize>>,
    ) -> Result<(), Error> {
        self.within(dir.extent()?, kept)?;
        for entry in dir.entries {
            if !entry.has_data() {
                continue;
            }
            let range = dir.range(entry)?;
            if entry.kind == directory::bios::RESET_IMAGE {
                if !entry.reset() || !entry.copy() {
                    return Err(Error::ResetFlags);
                }
                if reset == Some(&range) {
                    continue;
                }
            }
            self.within(range, kept)?;
        }
        Ok(())
    }
    fn validate_psp(
        &self,
        offset: usize,
        kept: usize,
        reset: Option<&Range<usize>>,
    ) -> Result<bool, Error> {
        let mut found_ish_bios = false;
        for dir in Levels::<PspEntry>::read_checked(*self, offset)?.iter() {
            self.within(dir.extent()?, kept)?;
            let mut seen_ish = [false; 2];
            for entry in dir.entries {
                if let Some(kind_idx) = match entry.kind {
                    directory::psp::RECOVERY_A => Some(0),
                    directory::psp::RECOVERY_B => Some(1),
                    _ => None,
                } {
                    if seen_ish[kind_idx] {
                        return Err(Error::DuplicateIshSlot(entry.kind));
                    }
                    seen_ish[kind_idx] = true;
                    let resolved = self.resolve_ish_entry(dir, entry)?;
                    let ish_end = resolved
                        .slot
                        .ish_offset
                        .checked_add(size_of::<directory::ImageSlotHeader>())
                        .ok_or(Error::Truncated(resolved.slot.ish_offset))?;
                    self.within(resolved.slot.ish_offset..ish_end, kept)?;
                    self.within(resolved.psp_l2.extent()?, kept)?;
                    for l2_entry in resolved.psp_l2.entries {
                        if !l2_entry.has_data() {
                            continue;
                        }
                        self.within(resolved.psp_l2.range(l2_entry)?, kept)?;
                    }
                    if let Some(bios) = resolved.bios {
                        self.within(bios.range, kept)?;
                        self.validate_bios_directory(bios.dir, kept, reset)?;
                        if let Some(l2) = bios.nested_l2 {
                            self.validate_bios_directory(l2, kept, reset)?;
                        }
                        found_ish_bios = true;
                    }
                    continue;
                }
                if !entry.has_data() {
                    continue;
                }
                let range = dir.range(entry)?;
                self.within(range.clone(), kept)?;
                if entry.kind == directory::psp::BIOS_TABLE {
                    let level = usize::from(
                        self.bytes[range.start..].starts_with(&directory::BIOS_LEVEL2_COOKIE),
                    );
                    let bios = Directory::<BiosEntry>::read_checked(*self, range.start, level)?;
                    if bios.table().len() > range.len() {
                        return Err(Error::Truncated(range.start));
                    }
                    self.validate_bios_directory(bios, kept, reset)?;
                    if level == 1
                        && bios
                            .entries
                            .iter()
                            .any(|e| e.kind == directory::bios::LEVEL2)
                    {
                        return Err(Error::NestedLevel2);
                    }
                    if let Some(l2) = bios.level2_checked()? {
                        self.validate_bios_directory(l2, kept, reset)?;
                    }
                }
            }
        }
        Ok(found_ish_bios)
    }
    fn within(&self, range: Range<usize>, kept: usize) -> Result<(), Error> {
        if range.end > kept {
            Err(Error::OutsidePrefix {
                start: range.start,
                end: range.end,
            })
        } else {
            Ok(())
        }
    }
}

struct ResolvedIshBios<'a> {
    range: Range<usize>,
    dir: Directory<'a, BiosEntry>,
    nested_l2: Option<Directory<'a, BiosEntry>>,
}

struct ResolvedIsh<'a> {
    slot: IshSlot,
    psp_l2: Directory<'a, PspEntry>,
    bios: Option<ResolvedIshBios<'a>>,
}

/// A validated Image Slot Header (`0x48` or `0x4A`) and its resolved
/// PSP L2 (`$PL2`) and optional BIOS L2 (`$BL2`) directory offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IshSlot {
    pub kind: u8,
    pub ish_offset: usize,
    pub header: directory::ImageSlotHeader,
    pub psp_l2_offset: usize,
    pub bios_l2_offset: Option<usize>,
}

/// Size of one flash half of a "Two 16M in 32MB" image (AMD #55758
/// §4.1.7).
pub const HALF_SIZE: usize = 16 << 20;

/// The FCH ROM2 window: the x86 view of the 16 MiB flash page below 4 GiB.
pub const ROM2_BASE: u64 = 0xff00_0000;

/// The flash half a boot ROM of `profile` boots: the first of `halves`
/// (offsets into `bytes`, searched in that order, lower first) whose EFS
/// it accepts, each half mapped below 4 GiB at `physical_base`.
pub fn booted_half(
    bytes: &[u8],
    physical_base: u64,
    profile: Profile,
    halves: &[usize],
) -> Option<usize> {
    halves.iter().copied().find(|&half| {
        half < bytes.len()
            && Flash::region(bytes, half, physical_base, profile)
                .efs_checked()
                .is_ok()
    })
}

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
