//! FFS builder — constructs firmware images (std only, used by xtask).
//!
//! The builder produces a complete firmware image:
//!
//! 1. Lays out regions (containers of files, raw reserved areas).
//! 2. For each container, lays out files and their segments.
//! 3. Computes digests for each file.
//! 4. Builds the `ImageManifest` with computed offsets.
//! 5. Serializes and signs the manifest.
//! 6. Builds the anchor block with embedded keys.
//! 7. Produces the final image as a `Vec<u8>`.
//!
//! Signing is done by accepting a closure — the builder doesn't know
//! about private keys directly (the xtask caller provides the signer).

extern crate std;

use std::string::String;
use std::vec::Vec;

use fstart_crypto::digest;
use fstart_types::ffs::{
    AnchorBlock, Compression, EntryContent, FileType, ImageManifest, Region, RegionContent,
    RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignedManifest, VerificationKey,
    ANCHOR_MAX_KEYS, ANCHOR_SIZE, FFS_MAGIC, FFS_VERSION,
};
use heapless::String as HString;

/// A file being assembled into the FFS image.
pub struct InputFile {
    /// File name.
    pub name: String,
    /// File type.
    pub file_type: FileType,
    /// Segments of this file, with their raw data.
    pub segments: Vec<InputSegment>,
}

/// A segment with its raw data, ready for inclusion in the image.
pub struct InputSegment {
    /// Segment name (e.g., ".text").
    pub name: String,
    /// Content kind.
    pub kind: SegmentKind,
    /// Raw uncompressed file data (`p_filesz` bytes; empty for pure BSS).
    pub data: Vec<u8>,
    /// Total memory size including BSS tail (`p_memsz`).
    ///
    /// When `mem_size > data.len()`, the loader must zero-fill the
    /// remaining `mem_size - data.len()` bytes after loading/decompressing.
    /// If `None`, defaults to `data.len()` (no BSS tail).
    pub mem_size: Option<u64>,
    /// Load address.
    pub load_addr: u64,
    /// Compression to apply.
    pub compression: Compression,
    /// Memory flags.
    pub flags: SegmentFlags,
}

/// A region to include in the image.
pub enum InputRegion {
    /// A container of files.
    Container {
        /// Region name (e.g., "ro", "rw-a").
        name: String,
        /// Files in this container.
        files: Vec<InputFile>,
    },
    /// Raw reserved space.
    Raw {
        /// Region name (e.g., "nvs").
        name: String,
        /// Size in bytes.
        size: u32,
        /// Fill byte (0xFF for erased flash).
        fill: u8,
    },
}

/// Configuration for building an FFS image.
pub struct FfsImageConfig {
    /// Verification keys to embed in the anchor.
    pub keys: Vec<VerificationKey>,
    /// Regions to include in the image, in order.
    ///
    /// The first Container region's first file must contain an embedded
    /// `FSTART_ANCHOR` placeholder (with `FFS_MAGIC` at an 8-byte-aligned
    /// offset) so the builder can find and patch it.
    pub regions: Vec<InputRegion>,
}

/// Location of a file's stored data within the built FFS image.
#[derive(Debug, Clone)]
pub struct FileDataLocation {
    /// File name (matches `InputFile::name`).
    pub name: String,
    /// Absolute byte offset in the image of the first segment's data.
    pub data_offset: u32,
    /// Total stored bytes across all segments (sum of `stored_size`).
    pub data_size: u32,
}

/// Result of building an FFS image.
pub struct FfsImage {
    /// The complete firmware image bytes.
    pub image: Vec<u8>,
    /// Offset of the anchor block in the image (for patching into bootblock).
    pub anchor_offset: usize,
    /// Serialized anchor block bytes (for embedding in bootblock static).
    pub anchor_bytes: Vec<u8>,
    /// Location of each file's data in the image (in layout order).
    ///
    /// Used by the assembler to find, e.g., the next stage's byte range
    /// for patching into the bootblock header.
    pub file_data: Vec<FileDataLocation>,
}

/// Build a bootable FFS image from the given configuration.
///
/// The first file in the first Container region is placed at offset 0 of
/// the image (making it directly bootable by QEMU `-bios`). Regions are
/// laid out sequentially. The signed manifest is appended at the end.
///
/// The first file must contain an embedded `FSTART_ANCHOR` placeholder
/// (with `FFS_MAGIC` at an 8-byte-aligned offset). The builder scans for
/// it and patches the anchor in-place with the real layout offsets.
///
/// `sign` is called to sign the manifest. It receives the raw manifest
/// bytes and must return a `Signature`.
pub fn build_image<F>(config: &FfsImageConfig, sign: &F) -> Result<FfsImage, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    let mut image: Vec<u8> = Vec::new();
    let mut file_data: Vec<FileDataLocation> = Vec::new();

    // ---- Phase 1: Lay out all regions ----
    let mut manifest_regions: heapless::Vec<Region, 4> = heapless::Vec::new();

    for input_region in &config.regions {
        match input_region {
            InputRegion::Container { name, files } => {
                let region_base = image.len() as u32;
                let mut children: heapless::Vec<RegionEntry, 16> = heapless::Vec::new();

                for file in files {
                    let entry = lay_out_file(&mut image, file, region_base)?;

                    // Record file data location for the assembler.
                    if let EntryContent::File { segments, .. } = &entry.content {
                        if let Some(first_seg) = segments.first() {
                            let abs_offset = region_base + entry.offset + first_seg.offset;
                            let total_stored: u32 = segments.iter().map(|s| s.stored_size).sum();
                            file_data.push(FileDataLocation {
                                name: file.name.clone(),
                                data_offset: abs_offset,
                                data_size: total_stored,
                            });
                        }
                    }

                    children
                        .push(entry)
                        .map_err(|_| "too many files in container (max 16)".to_string())?;
                }

                let region_size = image.len() as u32 - region_base;
                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset: region_base,
                        size: region_size,
                        content: RegionContent::Container { children },
                    })
                    .map_err(|_| "too many regions (max 4)".to_string())?;
            }
            InputRegion::Raw { name, size, fill } => {
                let offset = image.len() as u32;
                image.resize(image.len() + *size as usize, *fill);

                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset,
                        size: *size,
                        content: RegionContent::Raw { fill: *fill },
                    })
                    .map_err(|_| "too many regions (max 4)".to_string())?;
            }
        }
    }

    // ---- Phase 2: Build and sign the image manifest ----
    let manifest = ImageManifest {
        regions: manifest_regions,
    };

    let signed = sign_manifest(&manifest, sign)?;
    let manifest_offset = image.len() as u32;
    let manifest_serialized =
        postcard::to_allocvec(&signed).map_err(|e| format!("serialize manifest: {e}"))?;
    let manifest_size = manifest_serialized.len() as u32;
    image.extend_from_slice(&manifest_serialized);

    // ---- Phase 3: Build anchor and patch it into the bootblock binary ----
    if config.keys.len() > ANCHOR_MAX_KEYS {
        return Err(format!(
            "too many keys ({}, max {ANCHOR_MAX_KEYS})",
            config.keys.len()
        ));
    }
    let mut keys = [VerificationKey::ZERO; ANCHOR_MAX_KEYS];
    for (i, key) in config.keys.iter().enumerate() {
        keys[i] = *key;
    }

    // Scan the image for the FSTART01 placeholder. If none found (e.g.,
    // monolithic builds without FFS capabilities), reserve space at the end.
    let anchor_offset = match scan_for_magic(&image) {
        Ok(offset) => {
            if offset + ANCHOR_SIZE > image.len() {
                return Err(format!(
                    "anchor placeholder at offset {offset} would extend past image end (need {ANCHOR_SIZE} bytes)"
                ));
            }
            offset
        }
        Err(_) => {
            // No embedded placeholder — append space for the anchor at end
            let offset = image.len();
            image.resize(offset + ANCHOR_SIZE, 0);
            offset
        }
    };

    // Compute total_image_size *after* potentially appending the anchor,
    // so it accurately reflects the final image size.
    let total_image_size = image.len() as u32;

    let anchor = AnchorBlock {
        magic: FFS_MAGIC,
        version: FFS_VERSION,
        manifest_offset,
        manifest_size,
        total_image_size,
        key_count: config.keys.len() as u32,
        keys,
    };

    anchor.write_to(&mut image[anchor_offset..]);

    // ---- Phase 4: Recompute digests for the bootblock entry ----
    //
    // The anchor was patched into the bootblock after digests were computed,
    // so the bootblock's digest in the manifest is stale. Recompute it from
    // the actual image bytes, re-sign the manifest, and write it back.
    //
    // This works because the new serialized manifest has exactly the same
    // size — only the 32-byte hash values change inside fixed-size fields.
    let manifest = recompute_bootblock_digest(&image, &manifest)?;

    let new_signed = sign_manifest(&manifest, sign)?;
    let new_manifest_serialized =
        postcard::to_allocvec(&new_signed).map_err(|e| format!("re-serialize manifest: {e}"))?;

    if new_manifest_serialized.len() != manifest_size as usize {
        return Err(format!(
            "manifest size changed after digest recomputation ({} → {}); \
             this is a builder bug",
            manifest_size,
            new_manifest_serialized.len()
        ));
    }

    image[manifest_offset as usize..manifest_offset as usize + manifest_size as usize]
        .copy_from_slice(&new_manifest_serialized);

    // Return the raw anchor bytes for logging/debugging
    let mut anchor_bytes = vec![0u8; ANCHOR_SIZE];
    anchor.write_to(&mut anchor_bytes);

    Ok(FfsImage {
        image,
        anchor_offset,
        anchor_bytes,
        file_data,
    })
}

/// Scan the image for `FFS_MAGIC` at 8-byte-aligned offsets.
///
/// Returns the offset of the placeholder anchor in the bootblock binary.
fn scan_for_magic(image: &[u8]) -> Result<usize, String> {
    let magic = &FFS_MAGIC;
    let mut offset = 0;
    while offset + magic.len() <= image.len() {
        if &image[offset..offset + magic.len()] == magic {
            return Ok(offset);
        }
        offset += 8;
    }
    Err(
        "no FSTART01 magic found in image — the first file must be a bootblock \
         binary with an embedded FSTART_ANCHOR placeholder"
            .to_string(),
    )
}

/// Lay out a file's segments in the image, returning a `RegionEntry`.
fn lay_out_file(
    image: &mut Vec<u8>,
    file: &InputFile,
    region_base: u32,
) -> Result<RegionEntry, String> {
    let mut segments: heapless::Vec<Segment, 4> = heapless::Vec::new();
    let mut digest_input: Vec<u8> = Vec::new();

    let entry_offset = image.len() as u32 - region_base;

    for seg in &file.segments {
        // Align each segment to 8 bytes so embedded structures (like
        // the FSTART_ANCHOR in the bootblock) remain scannable at
        // 8-byte-aligned offsets. Without this, segments extracted
        // from ELF PT_LOAD headers land at arbitrary offsets.
        let pad = (8 - (image.len() % 8)) % 8;
        image.extend(core::iter::repeat_n(0u8, pad));

        let (stored_data, in_place_size, actual_compression) = match seg.compression {
            Compression::None => (seg.data.clone(), 0u32, Compression::None),
            Compression::Lz4 => {
                if seg.data.is_empty() {
                    // BSS or empty segments: nothing to compress
                    (Vec::new(), 0u32, Compression::None)
                } else {
                    let compressed = lz4_flex::block::compress(&seg.data);
                    if compressed.len() >= seg.data.len() {
                        // Compression didn't help (common for tiny segments
                        // like 1-byte .data). Store uncompressed instead.
                        (seg.data.clone(), 0u32, Compression::None)
                    } else {
                        let in_place = verify_in_place_lz4(
                            &compressed,
                            seg.data.len(),
                            &seg.name,
                            &file.name,
                        )?;
                        (compressed, in_place, Compression::Lz4)
                    }
                }
            }
        };

        // Record the offset relative to the entry base (which is relative to region base)
        let seg_offset = image.len() as u32 - region_base - entry_offset;

        // Append stored data to the image
        image.extend_from_slice(&stored_data);

        // Accumulate uncompressed data for whole-file digest
        digest_input.extend_from_slice(&seg.data);

        let name: HString<32> = HString::try_from(seg.name.as_str())
            .map_err(|_| format!("segment name too long: {}", seg.name))?;

        // loaded_size = mem_size if provided (includes BSS tail),
        // otherwise falls back to data.len() (no BSS).
        let loaded_size = seg
            .mem_size
            .map(|m| m as u32)
            .unwrap_or(seg.data.len() as u32);

        segments
            .push(Segment {
                name,
                kind: seg.kind,
                offset: seg_offset,
                stored_size: stored_data.len() as u32,
                loaded_size,
                in_place_size,
                load_addr: seg.load_addr,
                compression: actual_compression,
                flags: seg.flags,
            })
            .map_err(|_| format!("too many segments in file '{}'", file.name))?;
    }

    // Compute digests over concatenated uncompressed segment data
    let digests =
        digest::hash_digest_set(&digest_input).map_err(|_| "no digest algorithms available")?;

    let entry_size = image.len() as u32 - region_base - entry_offset;

    let name: HString<64> = HString::try_from(file.name.as_str())
        .map_err(|_| format!("file name too long: {}", file.name))?;

    Ok(RegionEntry {
        name,
        offset: entry_offset,
        size: entry_size,
        content: EntryContent::File {
            file_type: file.file_type,
            segments,
            digests,
        },
    })
}

/// Recompute the digest for the bootblock entry (first file in first container).
///
/// After the anchor is patched, the bootblock's on-image bytes differ from
/// the data that was hashed during layout. This function reads the actual
/// bytes from the image and updates the digest in the manifest.
fn recompute_bootblock_digest(
    image: &[u8],
    manifest: &ImageManifest,
) -> Result<ImageManifest, String> {
    let mut manifest = manifest.clone();

    // Find the first container region
    let region = manifest
        .regions
        .iter_mut()
        .find(|r| matches!(r.content, RegionContent::Container { .. }))
        .ok_or_else(|| "no container region found for digest recomputation".to_string())?;

    let region_offset = region.offset;

    let children = match &mut region.content {
        RegionContent::Container { children } => children,
        _ => unreachable!(),
    };

    if children.is_empty() {
        return Ok(manifest);
    }

    // The bootblock is the first entry in the first container
    let entry = &mut children[0];
    let entry_offset = entry.offset;

    let (segments, digests) = match &mut entry.content {
        EntryContent::File {
            segments, digests, ..
        } => (segments, digests),
        _ => return Ok(manifest),
    };

    // Re-read all segment data from the image (now with patched anchor)
    // and recompute the concatenated digest.
    //
    // Digests cover the *uncompressed* content of each segment. For the
    // bootblock, all segments must be uncompressed because the builder
    // patches the anchor directly into the image bytes. (Compressed
    // bootblock segments would corrupt the LZ4 stream.)
    let mut digest_input: Vec<u8> = Vec::new();
    for seg in segments.iter() {
        if seg.compression != Compression::None {
            return Err(format!(
                "bootblock segment '{}' uses {:?} compression — \
                 the bootblock must be uncompressed because the anchor \
                 is patched directly into its image bytes",
                seg.name, seg.compression,
            ));
        }
        let abs_offset = (region_offset + entry_offset + seg.offset) as usize;
        let end = abs_offset + seg.stored_size as usize;
        if end > image.len() {
            return Err(format!(
                "segment '{}' extends past image end during digest recomputation",
                seg.name
            ));
        }
        digest_input.extend_from_slice(&image[abs_offset..end]);
    }

    *digests =
        digest::hash_digest_set(&digest_input).map_err(|_| "no digest algorithms available")?;

    Ok(manifest)
}

/// Serialize a manifest, sign it, and return a `SignedManifest`.
fn sign_manifest<F>(manifest: &ImageManifest, sign: &F) -> Result<SignedManifest, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    let manifest_bytes_vec =
        postcard::to_allocvec(manifest).map_err(|e| format!("serialize manifest: {e}"))?;

    if manifest_bytes_vec.len() > 8192 {
        return Err(format!(
            "manifest too large ({} bytes, max 8192)",
            manifest_bytes_vec.len()
        ));
    }

    let mut manifest_bytes: heapless::Vec<u8, 8192> = heapless::Vec::new();
    manifest_bytes
        .extend_from_slice(&manifest_bytes_vec)
        .map_err(|_| "manifest bytes overflow".to_string())?;

    let signature = sign(&manifest_bytes_vec)?;

    Ok(SignedManifest {
        manifest_bytes,
        signature,
    })
}

/// Verify that an LZ4-compressed segment can be safely decompressed in-place.
///
/// Follows coreboot's cbfstool approach: simulate the in-place decompression
/// at build time. The compressed data is placed at the **end** of the output
/// buffer, then the decompressor writes from the beginning.
/// The decompressor reads from the tail while writing from the head; as long
/// as the buffer is large enough the write pointer never overtakes the read
/// pointer.
///
/// Returns the minimum contiguous buffer size (`in_place_size`) needed at
/// the load address, or an error if in-place decompression is not possible.
///
/// The worst-case margin is `8 + loaded_size / 255` bytes (from the LZ4 block
/// format: every 255 bytes of incompressible literals adds 1 byte of encoding
/// overhead, plus an 8-byte wild-copy guard). In practice the margin is much
/// smaller.
fn verify_in_place_lz4(
    compressed: &[u8],
    original_size: usize,
    seg_name: &str,
    file_name: &str,
) -> Result<u32, String> {
    if compressed.is_empty() || original_size == 0 {
        return Ok(0);
    }

    // Start with just loaded_size as the buffer size and increase if needed.
    // The theoretical worst-case overhead is 8 + original_size / 255 bytes,
    // but we verify empirically because the actual data may need less.
    let worst_case_margin = 8 + original_size / 255;
    let max_buf_size = original_size + worst_case_margin;

    // Try progressively larger buffers, starting from just `original_size`
    let mut buf_size = original_size;
    loop {
        // Simulate in-place decompression (coreboot cbfstool approach):
        // 1. Allocate a buffer of `buf_size` bytes
        // 2. Copy compressed data to the END of the buffer
        // 3. Decompress from tail to head
        //
        // Rust's borrow checker prevents simultaneous &[..] and &mut [..]
        // on the same buffer, so we use raw pointers for the simulation,
        // exactly as the runtime would.
        let mut buf = vec![0u8; buf_size];
        let src_offset = buf_size - compressed.len();
        buf[src_offset..].copy_from_slice(compressed);

        // Use our own overlap-safe decompressor — lz4_flex uses
        // copy_nonoverlapping internally which panics on overlapping ranges.
        let result = unsafe {
            let src = core::slice::from_raw_parts(buf.as_ptr().add(src_offset), compressed.len());
            let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), original_size);
            crate::lz4::decompress_block(src, dst)
        };

        match result {
            Ok(n) if n == original_size => {
                return Ok(buf_size as u32);
            }
            Ok(_) | Err(_) if buf_size < max_buf_size => {
                // Not enough scratch space — the in-place guard triggered
                // (Err) or match copies corrupted unread source data causing
                // a short decompression (Ok with wrong size). Try a larger
                // buffer so compressed data sits further from the output.
                buf_size += (original_size / 255).max(1);
                if buf_size > max_buf_size {
                    buf_size = max_buf_size;
                }
            }
            Ok(n) => {
                return Err(format!(
                    "LZ4 in-place decompression size mismatch for segment '{}' in \
                     file '{}': expected {original_size}, got {n} (even with \
                     worst-case buffer size {max_buf_size})",
                    seg_name, file_name,
                ));
            }
            Err(_) => {
                return Err(format!(
                    "LZ4 in-place decompression failed for segment '{}' in file '{}' \
                     even with worst-case buffer size ({max_buf_size} bytes = \
                     {original_size} + {worst_case_margin} margin)",
                    seg_name, file_name,
                ));
            }
        }
    }
}
