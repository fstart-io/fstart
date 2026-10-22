// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD PSP flash images, in the format of `fstart_amdfw`: [`assembly`]
//! composes a stage's image from a vendor image or packed blobs;
//! [`PicassoImage`] packs Picasso's first-generation PSP partition.
//!
//! Format parsing and validation live in the generation-independent
//! `fstart-amdfw-format` crate. This particular blob packer currently covers
//! Picasso; multi-level/ISH assembly is a separate coverage increment, not a
//! second parser. Signed AMD blobs are copied unchanged, never re-signed.
pub mod apcb;
pub mod assembly;
pub mod config;

use fstart_amdfw::{
    directory::{BIOS_COOKIE, BiosEntry, PSP_COOKIE, PspEntry, bios, psp, write_table},
    efs::{BiosDirectorySlot, Efs},
};
use zerocopy::IntoBytes;

pub type Result<T> = std::result::Result<T, String>;

/// A file entry's identity. Instance/subprogram are not interchangeable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub kind: u8,
    pub subprogram: u8,
    pub instance: u8,
}
impl Key {
    pub const fn new(kind: u8, subprogram: u8, instance: u8) -> Self {
        Self {
            kind,
            subprogram,
            instance,
        }
    }
    fn psp_entry(self, size: u32, address: u64) -> PspEntry {
        PspEntry::new(self.kind, self.subprogram, self.instance, size, address)
    }
    fn bios_entry(self, flags: u8, size: u32, source: u64, destination: u64) -> BiosEntry {
        BiosEntry::new(
            self.kind,
            self.subprogram,
            self.instance,
            flags,
            size,
            source,
            destination,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Blob {
    pub key: Key,
    pub bytes: Vec<u8>,
}

/// References describe existing flash allocations, never MMIO guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlashRegion {
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct PicassoImage {
    pub flash_size: u32,
    pub efs_offset: u32,
    /// Exclusive end of the firmware partition, not necessarily the chip end.
    pub partition_end: u32,
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    pub micron: bool,
    pub soft_fuse: u64,
    pub psp: Vec<Blob>,
    pub bios: Vec<Blob>,
    /// Raw, uncompressed reset image. PSP copies this into DRAM.
    pub reset_image: Vec<u8>,
    pub reset_destination: u64,
    pub apob_destination: u64,
    pub shared_memory: Option<(u64, u32)>,
    pub apob_nv: Option<FlashRegion>,
    pub psp_nv: Option<FlashRegion>,
}

#[derive(Debug)]
pub struct PackedImage {
    /// Only the assigned partition bytes, starting at `offset`. NOT a chip image.
    pub offset: u32,
    pub bytes: Vec<u8>,
    pub psp_directory: u32,
    pub bios_directory: u32,
}

struct Writer {
    base: u32,
    end: u32,
    bytes: Vec<u8>,
}
impl Writer {
    fn offset(&self) -> u32 {
        self.base + self.bytes.len() as u32
    }
    fn reserve(&mut self, size: usize) -> Result<u32> {
        let offset = self.offset();
        let next = u64::from(offset)
            .checked_add(size as u64)
            .ok_or("image overflow")?;
        if next > u64::from(self.end) {
            return Err("AMD firmware partition overflow".into());
        }
        self.bytes.resize(self.bytes.len() + size, 0xff);
        Ok(offset)
    }
    fn align(&mut self, alignment: u32) -> Result<()> {
        let padding = self.offset().wrapping_neg() & (alignment - 1);
        self.reserve(padding as usize)?;
        Ok(())
    }
    fn write(&mut self, offset: u32, bytes: &[u8]) {
        let start = (offset - self.base) as usize;
        self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
    }
    fn blob(&mut self, bytes: &[u8], alignment: u32) -> Result<(u32, u32)> {
        if bytes.is_empty() {
            return Err("empty AMD firmware blob".into());
        }
        self.align(alignment)?;
        let offset = self.reserve(bytes.len())?;
        self.write(offset, bytes);
        self.align(alignment)?;
        Ok((
            offset,
            u32::try_from(bytes.len()).map_err(|_| "blob too large")?,
        ))
    }
    fn directory<T: IntoBytes + zerocopy::Immutable>(
        &mut self,
        start: u32,
        cookie: [u8; 4],
        entries: &[T],
    ) -> Result<()> {
        self.align(0x1000)?;
        let blocks = (self.offset() - start) / 0x1000;
        if blocks > 0x3ff {
            return Err("Picasso directory exceeds 10-bit extent".into());
        }
        // Header v0, physical addresses, SPI block size 1, base_addr=0.
        let additional = blocks | (1 << 10);
        let at = (start - self.base) as usize;
        write_table(&mut self.bytes[at..], cookie, additional, entries)
            .ok_or("AMD directory outside the image")?;
        Ok(())
    }
}

fn validate_key(key: Key, bios: bool) -> Result<()> {
    if key.instance > 15 || (bios && key.subprogram > 7) || (!bios && key.instance != 0) {
        return Err(format!("invalid AMD entry selector {key:?}"));
    }
    // Gen1 APCB/backup selectors are instance-only. The source helper
    // creates backup entries with subprogram zero; accepting nonzero APCB
    // subprograms could create duplicate generated backup selectors.
    if bios && matches!(key.kind, bios::APCB | bios::APCB_BACKUP) && key.subprogram != 0 {
        return Err(format!("unsupported Picasso APCB selector {key:?}"));
    }
    let allowed = if bios {
        matches!(key.kind, 0x05 | 0x60 | 0x64..=0x66 | 0x68..=0x6a)
    } else {
        matches!(key.kind, 0x00..=0x05 | 0x08..=0x0a | 0x0c..=0x0d | 0x12..=0x13 |
                0x15 | 0x1a..=0x1d | 0x1f..=0x21 | 0x24..=0x25 | 0x28 | 0x2d |
                0x30..=0x3a | 0x3c | 0x42 | 0x44..=0x45 | 0x47 | 0x50..=0x55 |
                0x58..=0x5a | 0x5c..=0x5f)
    };
    if !allowed {
        return Err(format!("unsupported Picasso file type {key:?}"));
    }
    Ok(())
}
fn validate_blobs(blobs: &[Blob], bios: bool) -> Result<()> {
    let mut keys = std::collections::BTreeSet::new();
    for blob in blobs {
        validate_key(blob.key, bios)?;
        if !keys.insert(blob.key) {
            return Err(format!("duplicate AMD entry {:?}", blob.key));
        }
        if blob.bytes.is_empty() {
            return Err(format!("empty AMD entry {:?}", blob.key));
        }
    }
    Ok(())
}

impl PicassoImage {
    /// The EFS field Picasso's PSP takes the BIOS directory from.
    pub const BIOS_DIRECTORY: BiosDirectorySlot = BiosDirectorySlot::Family17hModel10;

    pub fn pack(&self) -> Result<PackedImage> {
        if !(0x40000..=0x1000000).contains(&self.flash_size) || !self.flash_size.is_power_of_two() {
            return Err("Picasso flash must be a power of two, 256 KiB..16 MiB".into());
        }
        // Hardware probes these offsets in its fixed 16-MiB mapping window.
        let mapped_efs = 0x1000000u32
            .checked_sub(self.flash_size)
            .and_then(|base| base.checked_add(self.efs_offset))
            .ok_or("invalid EFS offset")?;
        if ![0x20000, 0x820000, 0xc20000, 0xe20000, 0xf20000, 0xfa0000].contains(&mapped_efs) {
            return Err("EFS is not at a Picasso hardware search address".into());
        }
        if self.efs_offset >= self.partition_end || self.partition_end > self.flash_size {
            return Err("invalid AMD firmware partition".into());
        }
        if self.spi_read_mode > 7 || self.spi_speed > 3 {
            return Err("invalid EFS SPI read mode/speed".into());
        }
        if self.reset_image.is_empty()
            || self.reset_destination == 0
            || self
                .reset_destination
                .checked_add(self.reset_image.len() as u64)
                .is_none_or(|end| end > 0x1_0000_0000 || end & 0xffff != 0)
        {
            return Err("reset image must end on a 64-KiB DRAM boundary below 4 GiB".into());
        }
        if self.apob_destination == 0 {
            return Err("missing APOB DRAM destination".into());
        }
        validate_blobs(&self.psp, false)?;
        validate_blobs(&self.bios, true)?;
        // These external allocations must not intersect the generated partition or one another.
        let regions: Vec<_> = [self.apob_nv, self.psp_nv].into_iter().flatten().collect();
        for (i, region) in regions.iter().enumerate() {
            let end = region
                .offset
                .checked_add(region.size)
                .ok_or("NV range overflow")?;
            if region.size == 0
                || region.offset & 0xfff != 0
                || region.size & 0xfff != 0
                || end > self.flash_size
                || (region.offset < self.partition_end && end > self.efs_offset)
            {
                return Err(
                    "NV range must be erase-aligned, inside flash, outside firmware".into(),
                );
            }
            if regions[..i]
                .iter()
                .any(|r| region.offset < r.offset + r.size && end > r.offset)
            {
                return Err("overlapping NV ranges".into());
            }
        }
        if self.psp.iter().any(|b| b.key.kind == psp::NVRAM) && self.psp_nv.is_some() {
            return Err("PSP NVRAM has both a file and an external allocation".into());
        }
        let mut writer = Writer {
            base: self.efs_offset,
            end: self.partition_end,
            bytes: Vec::new(),
        };
        writer.reserve(0x100)?;
        // The first directory is 64 KiB aligned, not just 4 KiB.
        writer.align(0x10000)?;
        let psp_start = writer.reserve(0x1000)?; // 16 + 255*16
        let physical =
            |offset: u32| 0x1_0000_0000u64 - u64::from(self.flash_size) + u64::from(offset);
        let mut psp: Vec<_> = self.psp.iter().collect();
        psp.sort_by_key(|b| b.key);
        let mut psp_entries = Vec::new();
        let fuse = Key::new(psp::SOFT_FUSE, 0, 0).psp_entry(u32::MAX, self.soft_fuse);
        for blob in psp {
            if psp_entries
                .last()
                .is_some_and(|e: &PspEntry| e.kind < psp::SOFT_FUSE)
                && blob.key.kind > psp::SOFT_FUSE
            {
                psp_entries.push(fuse);
            }
            let nv = matches!(blob.key.kind, psp::NVRAM | psp::SEV_DATA | psp::RPMC_NVRAM);
            let (source, size) = writer.blob(&blob.bytes, if nv { 0x1000 } else { 0x100 })?;
            let size = if nv {
                size.checked_add(0xfff).ok_or("NV size overflow")? & !0xfff
            } else {
                size
            };
            psp_entries.push(blob.key.psp_entry(size, physical(source)));
        }
        if !psp_entries.iter().any(|e| e.kind == psp::SOFT_FUSE) {
            psp_entries.push(fuse);
        }
        if let Some(region) = self.psp_nv {
            psp_entries
                .push(Key::new(psp::NVRAM, 0, 0).psp_entry(region.size, physical(region.offset)));
        }
        psp_entries.sort_by_key(|e| (e.kind, e.subprogram));
        if psp_entries.len() > 255 {
            return Err("too many PSP entries".into());
        }
        writer.directory(psp_start, PSP_COOKIE, &psp_entries)?;

        let bios_start = writer.reserve(16 + 47 * 24)?;
        writer.align(0x1000)?;
        let mut bios_entries = Vec::new();
        let mut bios: Vec<_> = self.bios.iter().collect();
        // PMU instruction/data entries are interleaved per selector.
        bios.sort_by_key(|b| {
            (
                if b.key.kind == bios::PMU_DATA {
                    bios::PMU_INSTRUCTIONS
                } else {
                    b.key.kind
                },
                b.key.subprogram,
                b.key.instance,
                b.key.kind,
            )
        });
        for blob in bios.iter().filter(|b| b.key.kind == bios::RTM_PUBKEY) {
            let (src, size) = writer.blob(&blob.bytes, 0x100)?;
            bios_entries.push(blob.key.bios_entry(0, size, physical(src), u64::MAX));
        }
        // Picasso PSB signature reservation. All erased; no fake signature.
        let signature = writer.reserve(0x100)?;
        bios_entries.push(Key::new(bios::SIGNATURE, 0, 0).bios_entry(
            0,
            0x100,
            physical(signature),
            u64::MAX,
        ));
        for blob in bios.iter().filter(|b| b.key.kind == bios::APCB) {
            let (src, raw_size) = writer.blob(&blob.bytes, 0x1000)?;
            let size = raw_size.checked_add(0xfff).ok_or("APCB size overflow")? & !0xfff;
            bios_entries.push(blob.key.bios_entry(0, size, physical(src), u64::MAX));
            if !bios.iter().any(|b| b.key.kind == bios::APCB_BACKUP) {
                let mut backup = Key::new(bios::APCB_BACKUP, 0, blob.key.instance).bios_entry(
                    0,
                    size,
                    physical(src),
                    u64::MAX,
                );
                // add_bios_apcb_bk_entry leaves romid/writable/reserved erased.
                // Preserve those Gen1 bytes for reference-image equivalence.
                backup.subprogram = 0xf8;
                bios_entries.push(backup);
            }
        }
        // APOB size/source deliberately zero: only the destination is set.
        bios_entries.push(Key::new(bios::APOB, 0, 0).bios_entry(0, 0, 0, self.apob_destination));
        let (src, size) = writer.blob(&self.reset_image, 0x100)?;
        // Not compressed: the size is the DRAM extent.
        bios_entries.push(Key::new(bios::RESET_IMAGE, 0, 0).bios_entry(
            BiosEntry::RESET | BiosEntry::COPY,
            size,
            physical(src),
            self.reset_destination,
        ));
        if let Some(region) = self.apob_nv {
            let mut entry = Key::new(bios::APOB_NV, 0, 0).bios_entry(
                0,
                region.size,
                // Picasso's APOB NV quirk uses the fixed 16-MiB window base.
                0xff000000 + u64::from(region.offset),
                u64::MAX,
            );
            entry.subprogram = 1 << 5;
            bios_entries.push(entry);
        }
        for blob in bios
            .iter()
            .filter(|b| !matches!(b.key.kind, bios::RTM_PUBKEY | bios::APCB))
        {
            let (src, size) = writer.blob(
                &blob.bytes,
                if blob.key.kind == bios::APCB_BACKUP {
                    0x1000
                } else {
                    0x100
                },
            )?;
            let size = if blob.key.kind == bios::APCB_BACKUP {
                (size + 0xfff) & !0xfff
            } else {
                size
            };
            bios_entries.push(blob.key.bios_entry(0, size, physical(src), u64::MAX));
        }
        if let Some((destination, size)) = self.shared_memory {
            if destination == 0 || size == 0 || destination.checked_add(u64::from(size)).is_none() {
                return Err("invalid PSP shared DRAM extent".into());
            }
            // The shared-memory source bytes stay erased.
            bios_entries.push(Key::new(bios::SHARED_MEMORY, 0, 0).bios_entry(
                0,
                size,
                u64::MAX,
                destination,
            ));
        }
        if bios_entries.len() > 47 {
            return Err("too many BIOS entries".into());
        }
        writer.directory(bios_start, BIOS_COOKIE, &bios_entries)?;
        let mut efs = Efs::erased();
        // IMC, GEC and xHCI firmware not present.
        for pointer in [&mut efs.imc, &mut efs.gec, &mut efs.xhci] {
            pointer.set(0);
        }
        efs.new_psp_directory.set(physical(psp_start) as u32);
        efs.bios_directory_mut(PicassoImage::BIOS_DIRECTORY)
            .set(physical(bios_start) as u32);
        efs.spi_read_mode = self.spi_read_mode;
        efs.spi_speed = self.spi_speed;
        efs.qpr_dummy_cycle = if self.micron { 0x0a } else { 0xff };
        writer.write(self.efs_offset, efs.as_bytes());
        Ok(PackedImage {
            offset: self.efs_offset,
            bytes: writer.bytes,
            psp_directory: psp_start,
            bios_directory: bios_start,
        })
    }
}

#[cfg(test)]
mod tests;
