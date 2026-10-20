// SPDX-License-Identifier: MIT OR Apache-2.0
//! A complete flash image for AMD boards that keep the vendor's signed PSP
//! firmware: the vendor EFS, PSP/BIOS directories and blobs stay byte for
//! byte, the FFS goes behind them and the fstart stage replaces the BIOS reset
//! image (BIOS directory entry 0x62) that the PSP copies to DRAM.
//!
//! The vendor image is an explicit build input (`--image-config`). The
//! platform validates its directory geometry before planning; this module
//! re-checks the geometry against the bytes it composes.
use crate::build_plan::{FfsAssembly, FileBinding};
use fstart_amdfw::{Flash, directory::bios, efs::BiosDirectorySlot, efs::Efs};
use serde::{Deserialize, Serialize};
use zerocopy::IntoBytes;

/// Flash offsets of the composite image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorFlashLayout {
    pub flash_size: u32,
    /// Vendor bytes kept unchanged from offset 0.
    pub prefix_size: u32,
    pub ffs_offset: u32,
    pub ffs_size: u32,
    /// BIOS reset image: its flash source and the DRAM destination the PSP
    /// copies it to, from the vendor BIOS directory.
    pub reset_offset: u32,
    pub reset_size: u32,
    pub reset_destination: u64,
    /// The PSP output block destination (BIOS directory entry 0x61).
    pub apob_destination: u64,
    /// Where the SoC's PSP finds the BIOS directory.
    pub bios_directory: BiosDirectorySlot,
}
impl VendorFlashLayout {
    pub fn validate(&self) -> Result<(), String> {
        let end = |offset: u32, size: u32| offset.checked_add(size).ok_or("layout overflows");
        if self.prefix_size == 0
            || self.prefix_size > self.ffs_offset
            || end(self.ffs_offset, self.ffs_size)? > self.reset_offset
            || end(self.reset_offset, self.reset_size)? != self.flash_size
            || self.reset_destination % 0x1_0000 != 0
        {
            return Err("vendor flash layout is not prefix < FFS < top-aligned reset image".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorFlashAssembly {
    pub ffs: FfsAssembly,
    /// The vendor flash image the prefix is taken from.
    pub vendor: FileBinding,
    pub layout: VendorFlashLayout,
    /// Unit whose flat binary is the reset image.
    pub reset_unit: String,
}

/// BIOS directory entries the composition relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiosDirectoryFacts {
    /// The flash offset of the EFS.
    pub efs: usize,
    pub reset_offset: u32,
    pub reset_size: u32,
    pub reset_destination: u64,
    pub apob_destination: u64,
}

/// Read the reset image (type 0x62) and APOB (type 0x61) entries of the
/// BIOS directory the PSP of `slot` boots from in `image`: the EFS's, or the
/// level 2 it points to. The EFS, the directories and every file but the
/// reset image must lie within `prefix_size`, so composing can't cut off
/// vendor data, and every level must agree on the reset image.
pub fn read_bios_directory(
    image: &[u8],
    prefix_size: u32,
    slot: BiosDirectorySlot,
) -> Result<BiosDirectoryFacts, String> {
    let flash = Flash::new(image);
    let prefix = u64::from(prefix_size);
    let within = |offset: usize, size: usize| offset as u64 + size as u64 <= prefix;
    let (efs, _) = flash
        .efs()
        .filter(|&(offset, _)| within(offset, size_of::<Efs>()))
        .ok_or("no EFS in the vendor prefix")?;
    let levels = flash
        .bios_directories(slot)
        .ok_or("no valid BIOS directory for the SoC in the vendor image")?;
    let boot = levels.boot();
    let find = |kind| boot.entries.iter().find(|e| e.kind == kind);
    let reset = find(bios::RESET_IMAGE).ok_or("BIOS directory has no reset image (type 0x62)")?;
    let apob = find(bios::APOB).ok_or("BIOS directory has no APOB entry (type 0x61)")?;
    for directory in levels.iter() {
        if !within(directory.offset, directory.table().len()) {
            return Err("BIOS directory is outside the vendor prefix".into());
        }
        for entry in directory.entries {
            let (kind, size, source) = (entry.kind, entry.size.get(), entry.source.get());
            // Sources are either flash offsets or memory-mapped flash addresses.
            let offset = flash.offset(source).map_or(u64::MAX, |o| o as u64);
            if kind == bios::RESET_IMAGE {
                if entry.as_bytes() != reset.as_bytes() {
                    return Err("the BIOS directory levels disagree on the reset image".into());
                }
            } else if size != 0
                && source != 0
                && offset
                    .checked_add(u64::from(size))
                    .is_none_or(|end| end > prefix)
            {
                return Err(format!(
                    "BIOS directory entry {kind:#x} at {offset:#x} is outside the vendor prefix"
                ));
            }
        }
    }
    let reset_offset = flash
        .offset(reset.source.get())
        .and_then(|o| u32::try_from(o).ok())
        .ok_or("reset image source out of range")?;
    Ok(BiosDirectoryFacts {
        efs,
        reset_offset,
        reset_size: reset.size.get(),
        reset_destination: reset.destination.get(),
        apob_destination: apob.destination.get(),
    })
}

/// Compose the flash image. `reset` is the stage's flat binary of exactly
/// `layout.reset_size` bytes; its anchor and trust placeholders get the
/// FFS's finalized locator and verification policy.
pub fn compose(
    vendor: &[u8],
    layout: &VendorFlashLayout,
    ffs: &[u8],
    reset: &[u8],
) -> Result<Vec<u8>, String> {
    use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
    layout.validate()?;
    if vendor.len() != layout.flash_size as usize {
        return Err("vendor image size doesn't match the flash size".into());
    }
    let facts = read_bios_directory(vendor, layout.prefix_size, layout.bios_directory)?;
    if (
        facts.reset_offset,
        facts.reset_size,
        facts.reset_destination,
        facts.apob_destination,
    ) != (
        layout.reset_offset,
        layout.reset_size,
        layout.reset_destination,
        layout.apob_destination,
    ) {
        return Err("vendor BIOS directory doesn't match the planned layout".into());
    }
    if ffs.len() > layout.ffs_size as usize {
        return Err(format!(
            "FFS ({:#x} bytes) exceeds its flash window ({:#x})",
            ffs.len(),
            layout.ffs_size
        ));
    }
    if reset.len() != layout.reset_size as usize {
        return Err(format!(
            "reset image is {:#x} bytes, the BIOS directory entry {:#x}",
            reset.len(),
            layout.reset_size
        ));
    }
    let mut image = vec![0xff; layout.flash_size as usize];
    let prefix = layout.prefix_size as usize;
    image[..prefix].copy_from_slice(&vendor[..prefix]);
    let ffs_offset = layout.ffs_offset as usize;
    image[ffs_offset..ffs_offset + ffs.len()].copy_from_slice(ffs);
    let mut stage = reset.to_vec();

    // Locator: the stage's anchor placeholder gets the FFS anchor.
    let anchor_offset = fstart_ffs::FfsReader::new(ffs)
        .scan_for_anchor()
        .map_err(|e| format!("FFS anchor not found: {e:?}"))?;
    let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
    let placeholder = fstart_core::ffs::AnchorBlock::placeholder();
    // SAFETY: AnchorBlock is a plain repr(C) block of ANCHOR_SIZE bytes.
    let placeholder = unsafe {
        core::slice::from_raw_parts(
            &placeholder as *const fstart_core::ffs::AnchorBlock as *const u8,
            anchor_size,
        )
    };
    let site = find_unique(&stage, placeholder, 1).ok_or("no unique anchor site in the stage")?;
    stage[site..site + anchor_size]
        .copy_from_slice(&ffs[anchor_offset..anchor_offset + anchor_size]);

    // Verification policy: the stage's trust placeholder gets the FFS's.
    let mut trust_placeholder = [0; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut trust_placeholder);
    let trust = (0..ffs.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .filter(|&o| ffs[o..o + TRUST_SIZE] != trust_placeholder)
        .find_map(|o| TrustBlock::parse(&ffs[o..o + TRUST_SIZE]).filter(|b| b.key_count > 0))
        .ok_or("no finalized trust block in the FFS")?;
    let site =
        find_unique(&stage, &trust_placeholder, 8).ok_or("no unique trust site in the stage")?;
    trust.write_to(&mut stage[site..site + TRUST_SIZE]);

    let reset_offset = layout.reset_offset as usize;
    image[reset_offset..reset_offset + stage.len()].copy_from_slice(&stage);
    // The PSP must still find the vendor's EFS, not one in fstart's bytes.
    if read_bios_directory(&image, layout.prefix_size, layout.bios_directory)? != facts {
        return Err("the composed image has another BIOS directory".into());
    }
    Ok(image)
}

fn find_unique(haystack: &[u8], needle: &[u8], step: usize) -> Option<usize> {
    let mut sites = (0..haystack.len().saturating_sub(needle.len() - 1))
        .step_by(step)
        .filter(|&o| &haystack[o..o + needle.len()] == needle);
    let first = sites.next()?;
    sites.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_amdfw::directory::fletcher32;

    const SLOT: BiosDirectorySlot = BiosDirectorySlot::Family17hModel00;

    #[test]
    fn reads_the_mj11_reset_image_entry() {
        let facts = read_bios_directory(&two_level_vendor(), 0x29_4000, SLOT).unwrap();
        assert_eq!(
            facts,
            BiosDirectoryFacts {
                efs: 0x2_0000,
                reset_offset: 0xdb_0000,
                reset_size: 0x25_0000,
                reset_destination: 0x76db_0000,
                apob_destination: 0x3000_0000,
            }
        );
        // The level-2 microcode at 0x25f800 would be cut off by a smaller
        // prefix.
        assert!(read_bios_directory(&two_level_vendor(), 0x26_0000, SLOT).is_err());
        assert!(read_bios_directory(&two_level_vendor(), 0x26_0480, SLOT).is_ok());
    }

    /// Point level 2's entry `index` to `destination` and fix its checksum.
    fn set_level2_destination(image: &mut [u8], index: usize, destination: u64) {
        let entry = 0x22_9010 + index * 24;
        image[entry + 16..entry + 24].copy_from_slice(&destination.to_le_bytes());
        let checksum = fletcher32(&image[0x22_9008..0x22_9010 + 5 * 24]);
        image[0x22_9004..0x22_9008].copy_from_slice(&checksum.to_le_bytes());
    }

    #[test]
    fn the_psp_boots_from_level_2() {
        let mut image = two_level_vendor();
        // The APOB of level 2, not level 1's.
        set_level2_destination(&mut image, 1, 0x3100_0000);
        let facts = read_bios_directory(&image, 0x29_4000, SLOT).unwrap();
        assert_eq!(facts.apob_destination, 0x3100_0000);
        // The levels must agree on the reset image.
        set_level2_destination(&mut image, 2, 0x7600_0000);
        assert!(read_bios_directory(&image, 0x29_4000, SLOT).is_err());
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// An MJ11-like vendor image: the EFS, a level-1 BIOS directory that
    /// points to a level 2 and contents up to the prefix end.
    fn two_level_vendor() -> Vec<u8> {
        let mut image = vec![0xffu8; 0x100_0000];
        image[..0x29_4000]
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = ((i * 31) >> 8) as u8);
        let mut put = |offset: usize, bytes: &[u8]| {
            image[offset..offset + bytes.len()].copy_from_slice(bytes)
        };
        let efs: Vec<u8> = [0x55aa_55aa, 0, 0, 0, 0, 0xff05_7000, 0xff0f_7000]
            .into_iter()
            .chain([u32::MAX; 5])
            .flat_map(u32::to_le_bytes)
            .collect();
        put(0x2_0000, &efs);
        let mut directory =
            |offset: usize, cookie: &[u8; 4], entries: &[(u8, u8, u32, u64, u64)]| {
                let count = (entries.len() as u32).to_le_bytes();
                let mut table = [*cookie, [0; 4], count, [0; 4]].concat();
                for &(kind, flags, size, source, destination) in entries {
                    table.extend([kind, 0, flags, 0]);
                    table.extend(size.to_le_bytes());
                    table.extend(source.to_le_bytes());
                    table.extend(destination.to_le_bytes());
                }
                let checksum = fletcher32(&table[8..]);
                table[4..8].copy_from_slice(&checksum.to_le_bytes());
                put(offset, &table);
            };
        directory(
            0xf_7000,
            b"$BHD",
            &[
                (0x60, 0, 0x4000, 0xff0f_8000, u64::MAX),
                (0x61, 0, 0, 0, 0x3000_0000),
                (0x62, 3, 0x25_0000, 0xffdb_0000, 0x76db_0000),
                (0x70, 0, 0x400, 0xff22_9000, u64::MAX),
            ],
        );
        directory(
            0x22_9000,
            b"$BL2",
            &[
                (0x60, 0, 0x1d00, 0xff22_a000, u64::MAX),
                (0x61, 0, 0, 0, 0x3000_0000),
                (0x62, 3, 0x25_0000, 0xffdb_0000, 0x76db_0000),
                (0x63, 0, 0x2_0000, 0xff22_e000, u64::MAX),
                (0x66, 0, 0xc80, 0xff25_f800, u64::MAX),
            ],
        );
        image
    }

    /// An FFS with its locator and a finalized trust block, and a stage with
    /// the placeholders of both.
    fn ffs_and_stage(reset_size: usize) -> (Vec<u8>, Vec<u8>) {
        use fstart_core::ffs::{AnchorBlock, trust::TrustBlock};
        let pattern = |len: usize, seed: usize| -> Vec<u8> {
            (0..len).map(|i| ((i * seed) >> 4) as u8).collect()
        };
        let mut ffs = pattern(0x2000, 7);
        let mut locator = AnchorBlock::placeholder();
        locator.manifest_offset.set(0x100);
        locator.manifest_size.set(0x20);
        locator.write_to(&mut ffs[0x40..]);
        let mut trust = TrustBlock::placeholder();
        trust.set_key_count(1);
        trust.write_to(&mut ffs[0x400..]);
        let mut stage = pattern(reset_size, 13);
        AnchorBlock::placeholder().write_to(&mut stage[0x1000..]);
        TrustBlock::placeholder().write_to(&mut stage[0x2000..]);
        (ffs, stage)
    }

    #[test]
    fn composes_the_pinned_image() {
        let layout = VendorFlashLayout {
            flash_size: 0x100_0000,
            prefix_size: 0x29_4000,
            ffs_offset: 0x29_4000,
            ffs_size: 0xdb_0000 - 0x29_4000,
            reset_offset: 0xdb_0000,
            reset_size: 0x25_0000,
            reset_destination: 0x76db_0000,
            apob_destination: 0x3000_0000,
            bios_directory: SLOT,
        };
        let (ffs, stage) = ffs_and_stage(0x25_0000);
        let vendor = two_level_vendor();
        let image = compose(&vendor, &layout, &ffs, &stage).unwrap();
        assert_eq!(image[..0x29_4000], vendor[..0x29_4000]);
        assert_eq!(image[0x29_4000..0x29_6000], ffs);
        // The stage got the FFS's locator.
        assert_eq!(image[0xdb_1000..0xdb_1028], ffs[0x40..0x68]);
        assert_eq!(
            hex(&fstart_crypto::digest::hash_sha256(&image)),
            "021a94e0a1b68bfabee168c750b1e6c1dd48c5b607f8c77729a286d37a475488"
        );
    }
}
