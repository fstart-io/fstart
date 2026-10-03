// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso's first-generation PSP image format.
//!
//! This deliberately does not pretend to support newer multi-level/ISH formats.
//! Signed AMD blobs are copied unchanged; this is a packer, not a signing tool.
pub mod apcb;
pub mod config;
mod wire;

use wire::*;
use zerocopy::IntoBytes;

pub type Result<T> = std::result::Result<T, String>;

/// A file entry's identity. Instance/subprogram are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
}

#[derive(Debug, Clone)]
pub struct Blob {
    pub key: Key,
    pub bytes: Vec<u8>,
}

/// References describe existing flash allocations, never MMIO guesses.
#[derive(Debug, Clone, Copy)]
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
        cookie: u32,
        entries: &[T],
    ) -> Result<()> {
        self.align(0x1000)?;
        let blocks = (self.offset() - start) / 0x1000;
        if blocks > 0x3ff {
            return Err("Picasso directory exceeds 10-bit extent".into());
        }
        let mut header = Header {
            cookie: cookie.into(),
            checksum: 0.into(),
            count: (entries.len() as u32).into(),
            // Header v0, physical addresses, SPI block size 1, base_addr=0.
            additional: (blocks | (1 << 10)).into(),
        };
        let mut table = header.as_bytes().to_vec();
        for entry in entries {
            table.extend_from_slice(entry.as_bytes());
        }
        header.checksum = fletcher32(&table[8..]).into();
        table[..16].copy_from_slice(header.as_bytes());
        self.write(start, &table);
        Ok(())
    }
}

fn validate_blobs(blobs: &[Blob], bios: bool) -> Result<()> {
    let mut keys = std::collections::BTreeSet::new();
    for blob in blobs {
        let key = blob.key;
        if !keys.insert(key) {
            return Err(format!("duplicate AMD entry {key:?}"));
        }
        if key.instance > 15 || (bios && key.subprogram > 7) || (!bios && key.instance != 0) {
            return Err(format!("invalid AMD entry selector {key:?}"));
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
        if blob.bytes.is_empty() {
            return Err(format!("empty AMD entry {key:?}"));
        }
    }
    Ok(())
}

impl PicassoImage {
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
        if self.psp.iter().any(|b| b.key.kind == 4) && self.psp_nv.is_some() {
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
        let fuse = PspEntry::new(Key::new(0x0b, 0, 0), u32::MAX, self.soft_fuse);
        for blob in psp {
            if psp_entries.last().is_some_and(|e: &PspEntry| e.kind < 0x0b) && blob.key.kind > 0x0b
            {
                psp_entries.push(fuse);
            }
            let nv = matches!(blob.key.kind, 4 | 0x38 | 0x54);
            let (source, size) = writer.blob(&blob.bytes, if nv { 0x1000 } else { 0x100 })?;
            let size = if nv {
                size.checked_add(0xfff).ok_or("NV size overflow")? & !0xfff
            } else {
                size
            };
            psp_entries.push(PspEntry::new(blob.key, size, physical(source)));
        }
        if !psp_entries.iter().any(|e| e.kind == 0x0b) {
            psp_entries.push(fuse);
        }
        if let Some(region) = self.psp_nv {
            psp_entries.push(PspEntry::new(
                Key::new(4, 0, 0),
                region.size,
                physical(region.offset),
            ));
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
                if b.key.kind == 0x65 { 0x64 } else { b.key.kind },
                b.key.subprogram,
                b.key.instance,
                b.key.kind,
            )
        });
        for blob in bios.iter().filter(|b| b.key.kind == 5) {
            let (src, size) = writer.blob(&blob.bytes, 0x100)?;
            bios_entries.push(BiosEntry::new(blob.key, 0, size, physical(src), u64::MAX));
        }
        // Picasso PSB signature reservation. All erased; no fake signature.
        let signature = writer.reserve(0x100)?;
        bios_entries.push(BiosEntry::new(
            Key::new(7, 0, 0),
            0,
            0x100,
            physical(signature),
            u64::MAX,
        ));
        for blob in bios.iter().filter(|b| b.key.kind == 0x60) {
            let (src, raw_size) = writer.blob(&blob.bytes, 0x1000)?;
            let size = raw_size.checked_add(0xfff).ok_or("APCB size overflow")? & !0xfff;
            bios_entries.push(BiosEntry::new(blob.key, 0, size, physical(src), u64::MAX));
            if !bios.iter().any(|b| b.key.kind == 0x68) {
                let mut backup = BiosEntry::new(
                    Key::new(0x68, 0, blob.key.instance),
                    0,
                    size,
                    physical(src),
                    u64::MAX,
                );
                // add_bios_apcb_bk_entry leaves romid/writable/reserved erased.
                // Preserve those Gen1 bytes for reference-image equivalence.
                backup.subprogram_rom = 0xf8;
                bios_entries.push(backup);
            }
        }
        // APOB size/source deliberately zero: only the destination is set.
        bios_entries.push(BiosEntry::new(
            Key::new(0x61, 0, 0),
            0,
            0,
            0,
            self.apob_destination,
        ));
        let (src, size) = writer.blob(&self.reset_image, 0x100)?;
        // reset=1, copy=1, compressed=0. Size is the uncompressed DRAM extent.
        bios_entries.push(BiosEntry::new(
            Key::new(0x62, 0, 0),
            3,
            size,
            physical(src),
            self.reset_destination,
        ));
        if let Some(region) = self.apob_nv {
            let mut entry = BiosEntry::new(
                Key::new(0x63, 0, 0),
                0,
                region.size,
                // Picasso's APOB NV quirk uses the fixed 16-MiB window base.
                0xff000000 + u64::from(region.offset),
                u64::MAX,
            );
            entry.subprogram_rom = 1 << 5;
            bios_entries.push(entry);
        }
        for blob in bios.iter().filter(|b| !matches!(b.key.kind, 5 | 0x60)) {
            let (src, size) = writer.blob(
                &blob.bytes,
                if blob.key.kind == 0x68 { 0x1000 } else { 0x100 },
            )?;
            let size = if blob.key.kind == 0x68 {
                (size + 0xfff) & !0xfff
            } else {
                size
            };
            bios_entries.push(BiosEntry::new(blob.key, 0, size, physical(src), u64::MAX));
        }
        if let Some((destination, size)) = self.shared_memory {
            if destination == 0 || size == 0 || destination.checked_add(u64::from(size)).is_none() {
                return Err("invalid PSP shared DRAM extent".into());
            }
            // The shared-memory source bytes stay erased.
            bios_entries.push(BiosEntry::new(
                Key::new(0x6b, 0, 0),
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
        let mut efs = [0xffu8; 0x60];
        efs[..4].copy_from_slice(&0x55aa55aau32.to_le_bytes());
        efs[4..16].fill(0); // IMC/GEC/xHCI not present
        efs[0x14..0x18].copy_from_slice(&(physical(psp_start) as u32).to_le_bytes());
        efs[0x1c..0x20].copy_from_slice(&(physical(bios_start) as u32).to_le_bytes());
        efs[0x43] = self.spi_read_mode;
        efs[0x44] = self.spi_speed;
        efs[0x45] = if self.micron { 0x0a } else { 0xff };
        writer.write(self.efs_offset, &efs);
        Ok(PackedImage {
            offset: self.efs_offset,
            bytes: writer.bytes,
            psp_directory: psp_start,
            bios_directory: bios_start,
        })
    }
}

/// The folded Fletcher-32, little-endian 16-bit words, 359-word chunks.
pub fn fletcher32(bytes: &[u8]) -> u32 {
    assert!(bytes.len().is_multiple_of(2));
    let (mut c0, mut c1) = (0xffffu32, 0xffffu32);
    for chunk in bytes.chunks(359 * 2) {
        for word in chunk.chunks_exact(2) {
            c0 += u32::from(u16::from_le_bytes([word[0], word[1]]));
            c1 += c0;
        }
        c0 = (c0 & 0xffff) + (c0 >> 16);
        c1 = (c1 & 0xffff) + (c1 >> 16);
    }
    c0 = (c0 & 0xffff) + (c0 >> 16);
    c1 = (c1 & 0xffff) + (c1 >> 16);
    (c1 << 16) | c0
}

#[cfg(test)]
mod tests;
