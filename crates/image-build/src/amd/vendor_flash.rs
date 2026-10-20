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
use serde::{Deserialize, Serialize};

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
    pub reset_offset: u32,
    pub reset_size: u32,
    pub reset_destination: u64,
    pub apob_destination: u64,
}

/// Find the (first-level) BIOS directory in `image` and read the reset
/// image (type 0x62) and APOB (type 0x61) entries. Every other entry with
/// flash contents must lie within `prefix_size`, so composing can't cut off
/// vendor data.
pub fn read_bios_directory(image: &[u8], prefix_size: u32) -> Result<BiosDirectoryFacts, String> {
    const COOKIE: &[u8; 4] = b"$BHD";
    let flash_base = 0x1_0000_0000u64
        .checked_sub(image.len() as u64)
        .ok_or("vendor image larger than 4 GiB")?;
    let u32_at = |o: usize| u32::from_le_bytes(image[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(image[o..o + 8].try_into().unwrap());
    let directory = (0..prefix_size as usize)
        .step_by(0x1000)
        .find(|&o| o + 16 <= image.len() && &image[o..o + 4] == COOKIE)
        .ok_or("no BIOS directory in the vendor prefix")?;
    let count = u32_at(directory + 8) as usize;
    if count == 0 || count > 64 || directory + 16 + count * 24 > prefix_size as usize {
        return Err("BIOS directory entry count is out of range".into());
    }
    let mut reset = None;
    let mut apob = None;
    for entry in (0..count).map(|i| directory + 16 + i * 24) {
        let kind = image[entry];
        let size = u32_at(entry + 4);
        let source = u64_at(entry + 8);
        let destination = u64_at(entry + 16);
        // Sources are either flash offsets or memory-mapped flash addresses.
        let offset = source.checked_sub(flash_base).unwrap_or(source);
        match kind {
            0x61 => apob = Some(destination),
            0x62 => {
                reset = Some((
                    u32::try_from(offset).map_err(|_| "reset image source out of range")?,
                    size,
                    destination,
                ))
            }
            _ if size != 0 && source != 0 && offset + u64::from(size) > u64::from(prefix_size) => {
                return Err(format!(
                    "BIOS directory entry {kind:#x} at {offset:#x} is outside the vendor prefix"
                ));
            }
            _ => {}
        }
    }
    let (reset_offset, reset_size, reset_destination) =
        reset.ok_or("BIOS directory has no reset image (type 0x62)")?;
    Ok(BiosDirectoryFacts {
        reset_offset,
        reset_size,
        reset_destination,
        apob_destination: apob.ok_or("BIOS directory has no APOB entry (type 0x61)")?,
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
    let facts = read_bios_directory(vendor, layout.prefix_size)?;
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

    fn vendor() -> Vec<u8> {
        let mut image = vec![0xffu8; 0x100_0000];
        let dir = 0xf7000;
        image[dir..dir + 4].copy_from_slice(b"$BHD");
        image[dir + 8..dir + 12].copy_from_slice(&3u32.to_le_bytes());
        let mut entry = |i: usize, kind: u8, size: u32, source: u64, dest: u64| {
            let e = dir + 16 + i * 24;
            image[e..e + 24].fill(0);
            image[e] = kind;
            image[e + 4..e + 8].copy_from_slice(&size.to_le_bytes());
            image[e + 8..e + 16].copy_from_slice(&source.to_le_bytes());
            image[e + 16..e + 24].copy_from_slice(&dest.to_le_bytes());
        };
        entry(0, 0x60, 0x4000, 0xff0f_8000, u64::MAX);
        entry(1, 0x61, 0, 0, 0x3000_0000);
        entry(2, 0x62, 0x25_0000, 0xffdb_0000, 0x76db_0000);
        image
    }

    #[test]
    fn reads_the_mj11_reset_image_entry() {
        let facts = read_bios_directory(&vendor(), 0x29_4000).unwrap();
        assert_eq!(
            facts,
            BiosDirectoryFacts {
                reset_offset: 0xdb_0000,
                reset_size: 0x25_0000,
                reset_destination: 0x76db_0000,
                apob_destination: 0x3000_0000,
            }
        );
        // The APCB backup at 0xf8000 would be cut off by a smaller prefix.
        assert!(read_bios_directory(&vendor(), 0xf8000).is_err());
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
                let checksum = crate::amd::fletcher32(&table[8..]);
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
