//! FFS builder — constructs firmware images (std only, used by fbuild).
//!
//! The builder produces a complete firmware image:
//!
//! 1. Lays out regions (containers of files, raw reserved areas).
//! 2. For each container, lays out files and their segments.
//! 3. Computes digests for each file.
//! 4. Builds the `ImageManifest` with computed offsets.
//! 5. Serializes the directory and signs its bounded boot root.
//! 6. Builds the anchor block with embedded keys.
//! 7. Produces the final image as a `Vec<u8>`.
//!
//! Signing is done by accepting a closure — the builder doesn't know
//! about private keys directly (the fstart-image-build caller provides the signer).

extern crate std;

use std::string::String;
use std::vec::Vec;

use crate::root::{BootstrapDescriptor, BootstrapRole, DirectoryRef, ROOT_SIZE, Root, SIGNED_SIZE};
use fstart_core::ffs::{
    ANCHOR_MAX_KEYS, ANCHOR_SIZE, AnchorBlock, Compression, EntryContent, FFS_MAGIC, FFS_VERSION,
    FileType, ImageManifest, Region, RegionContent, RegionEntry, Segment, SegmentFlags,
    SegmentKind, Signature, VerificationKey,
};
use fstart_crypto::digest;
use heapless::String as HString;

/// Protected packaging policy. Names select flat stage files, never arbitrary offsets.
#[derive(Default)]
pub struct BootRootConfig {
    pub image_family: [u8; 16],
    pub security_version: u64,
    pub bootstrap: Vec<(String, BootstrapRole)>,
}

/// A file being assembled into the FFS image.
pub struct InputFile {
    /// File name.
    pub name: String,
    /// File type.
    pub file_type: FileType,
    /// Segments of this file, with their raw data.
    pub segments: Vec<InputSegment>,
}

/// A file described by the manifest but stored outside the sequential FFS blob.
///
/// This is used for XIP bootblocks that are top-aligned in flash. The manifest
/// records their real image-relative offset and digest, while the bytes are
/// supplied separately by the full-flash assembler.
pub struct ExternalInputFile {
    /// File name.
    pub name: String,
    /// File type.
    pub file_type: FileType,
    /// Offset from the parent region's base.
    pub offset: u32,
    /// Segments of this file, with their raw data for digest generation.
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
    /// A container with additional externally placed files.
    ContainerWithExternal {
        /// Region name (e.g., "ro").
        name: String,
        /// Files physically stored in this image blob.
        files: Vec<InputFile>,
        /// Files described by the manifest but placed by a full-flash assembler.
        external_files: Vec<ExternalInputFile>,
        /// Optional explicit region size.
        size: Option<u32>,
    },
    /// Raw reserved space that is physically present in this image.
    Raw {
        /// Region name (e.g., "nvs").
        name: String,
        /// Size in bytes.
        size: u32,
        /// Fill byte (0xFF for erased flash).
        fill: u8,
    },
    /// Raw flash region described in the manifest but not stored in this image.
    ///
    /// This is used for descriptor-based x86 platforms where a BIOS-region
    /// image needs to carry signed layout metadata for descriptor/GbE/ME
    /// regions without embedding those vendor blobs or computing digests for
    /// them.
    ExternalRaw {
        /// Region name (e.g., "descriptor", "gbe", "me").
        name: String,
        /// Region offset in the physical flash layout.
        offset: u32,
        /// Region size in bytes.
        size: u32,
        /// Erased fill byte convention for the region.
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
/// laid out sequentially. The directory and signed boot root are appended.
///
/// The first file must contain an embedded `FSTART_ANCHOR` placeholder
/// (with `FFS_MAGIC` at an 8-byte-aligned offset). The builder scans for
/// it and patches the anchor in-place with the real layout offsets.
///
/// This convenience entry uses the development family ID and no bootstrap
/// descriptors. Real board packaging calls `build_image_with_root` with explicit
/// policy. `sign` receives the first 448 root bytes and must return Ed25519.
pub fn build_image<F>(config: &FfsImageConfig, sign: &F) -> Result<FfsImage, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    build_image_with_root(config, &BootRootConfig::default(), sign)
}

pub fn build_image_with_root<F>(
    config: &FfsImageConfig,
    root_config: &BootRootConfig,
    sign: &F,
) -> Result<FfsImage, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    if root_config.bootstrap.len() > 2 {
        return Err("boot root supports at most two bootstrap stages".into());
    }
    for (i, key) in config.keys.iter().enumerate() {
        if config.keys[..i]
            .iter()
            .any(|prev| prev.key_id == key.key_id)
        {
            return Err("duplicate authorized key ID".into());
        }
    }
    for (i, (name, role)) in root_config.bootstrap.iter().enumerate() {
        if root_config.bootstrap[..i]
            .iter()
            .any(|(previous, previous_role)| previous == name || previous_role == role)
        {
            return Err("duplicate bootstrap name or role".into());
        }
    }
    let mut image: Vec<u8> = Vec::new();
    let mut file_data: Vec<FileDataLocation> = Vec::new();

    // ---- Phase 1: Lay out all regions ----
    let mut manifest_regions: heapless::Vec<Region, 6> = heapless::Vec::new();

    for input_region in &config.regions {
        match input_region {
            InputRegion::Container { name, files } => {
                let region_base = image_len(&image)?;
                let mut children: heapless::Vec<RegionEntry, 8> = heapless::Vec::new();

                for file in files {
                    let entry = lay_out_file(&mut image, file, region_base)?;

                    // Record file data location for the assembler.
                    if let EntryContent::File { segments, .. } = &entry.content
                        && let Some(first_seg) = segments.first()
                    {
                        let abs_offset = region_base
                            .checked_add(entry.offset)
                            .and_then(|n| n.checked_add(first_seg.offset))
                            .ok_or("file offset exceeds u32")?;
                        let total_stored = segments
                            .iter()
                            .try_fold(0u32, |sum, s| sum.checked_add(s.stored_size))
                            .ok_or("stored file exceeds u32")?;
                        file_data.push(FileDataLocation {
                            name: file.name.clone(),
                            data_offset: abs_offset,
                            data_size: total_stored,
                        });
                    }

                    children
                        .push(entry)
                        .map_err(|_| "too many files in container (max 8)".to_string())?;
                }

                let region_size = image_len(&image)? - region_base;
                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset: region_base,
                        size: region_size,
                        content: RegionContent::Container { children },
                    })
                    .map_err(|_| "too many regions (max 6)".to_string())?;
            }
            InputRegion::ContainerWithExternal {
                name,
                files,
                external_files,
                size,
            } => {
                let region_base = image_len(&image)?;
                let mut children: heapless::Vec<RegionEntry, 8> = heapless::Vec::new();

                for file in files {
                    let entry = lay_out_file(&mut image, file, region_base)?;

                    if let EntryContent::File { segments, .. } = &entry.content
                        && let Some(first_seg) = segments.first()
                    {
                        let abs_offset = region_base
                            .checked_add(entry.offset)
                            .and_then(|n| n.checked_add(first_seg.offset))
                            .ok_or("file offset exceeds u32")?;
                        let total_stored = segments
                            .iter()
                            .try_fold(0u32, |sum, s| sum.checked_add(s.stored_size))
                            .ok_or("stored file exceeds u32")?;
                        file_data.push(FileDataLocation {
                            name: file.name.clone(),
                            data_offset: abs_offset,
                            data_size: total_stored,
                        });
                    }

                    children
                        .push(entry)
                        .map_err(|_| "too many files in container (max 8)".to_string())?;
                }

                let mut region_size = image_len(&image)? - region_base;
                for file in external_files {
                    let entry = external_file_entry(file)?;
                    region_size = region_size.max(
                        entry
                            .offset
                            .checked_add(entry.size)
                            .ok_or("external file range exceeds u32")?,
                    );
                    children
                        .push(entry)
                        .map_err(|_| "too many files in container (max 8)".to_string())?;
                }
                if let Some(size) = size {
                    region_size = region_size.max(*size);
                }

                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset: region_base,
                        size: region_size,
                        content: RegionContent::Container { children },
                    })
                    .map_err(|_| "too many regions (max 6)".to_string())?;
            }
            InputRegion::Raw { name, size, fill } => {
                let offset = image_len(&image)?;
                image.resize(
                    offset.checked_add(*size).ok_or("raw region exceeds u32")? as usize,
                    *fill,
                );

                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset,
                        size: *size,
                        content: RegionContent::Raw { fill: *fill },
                    })
                    .map_err(|_| "too many regions (max 6)".to_string())?;
            }
            InputRegion::ExternalRaw {
                name,
                offset,
                size,
                fill,
            } => {
                let region_name: HString<64> = HString::try_from(name.as_str())
                    .map_err(|_| format!("region name too long: {name}"))?;

                manifest_regions
                    .push(Region {
                        name: region_name,
                        offset: *offset,
                        size: *size,
                        content: RegionContent::Raw { fill: *fill },
                    })
                    .map_err(|_| "too many regions (max 6)".to_string())?;
            }
        }
    }

    // ---- Phase 2: Reserve fixed directory and boot-root storage ----
    let mut manifest = ImageManifest {
        regions: manifest_regions,
    };

    let directory_bytes = crate::manifest::encode_manifest(&manifest)?;
    let directory_offset = u32::try_from(image.len()).map_err(|_| "image exceeds u32")?;
    image.extend_from_slice(&directory_bytes);
    let manifest_offset = u32::try_from(image.len()).map_err(|_| "image exceeds u32")?;
    let manifest_size = ROOT_SIZE as u32;
    image.resize(image.len() + ROOT_SIZE, 0);

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

    // Scan the image for every `FSTART_ANCHOR` placeholder. Multi-stage
    // images contain one placeholder per FFS-using stage; all must be
    // patched because later stages reference their own embedded static.
    // If no placeholder is found, append one at the end as a fallback.
    let mut anchor_offsets = scan_for_placeholders(&image);
    if anchor_offsets.is_empty() {
        let pad = (8 - (image.len() % 8)) % 8;
        image.extend(core::iter::repeat_n(0u8, pad));
        let offset = image.len();
        image.resize(offset + ANCHOR_SIZE, 0);
        anchor_offsets.push(offset);
    }
    let anchor_offset = anchor_offsets[0];

    // Compute total_image_size *after* potentially appending the anchor,
    // so it accurately reflects the final image size.
    let total_image_size = image_len(&image)?;
    let microcode = file_data
        .iter()
        .find(|file| file.name == "cpu_microcode_blob.bin");

    let anchor = AnchorBlock {
        magic: FFS_MAGIC,
        version: FFS_VERSION,
        manifest_offset,
        manifest_size,
        total_image_size,
        anchor_offset: u32::try_from(anchor_offset).map_err(|_| "anchor offset exceeds u32")?,
        microcode_offset: microcode.map_or(0, |file| file.data_offset),
        microcode_size: microcode.map_or(0, |file| file.data_size),
        key_count: config.keys.len() as u32,
        keys,
        image_family: root_config.image_family,
    };

    for &offset in &anchor_offsets {
        if offset + ANCHOR_SIZE > image.len() {
            return Err(format!(
                "anchor placeholder at offset {offset} would extend past image end (need {ANCHOR_SIZE} bytes)"
            ));
        }
        anchor.write_to(&mut image[offset..]);
    }

    // ---- Phase 4: Recompute digests for files containing patched anchors ----
    //
    // Anchors are patched after digests were computed, so each containing
    // file's digest in the manifest is stale.
    recompute_file_digests(
        &image,
        &mut manifest,
        &anchor_offsets,
        &config.regions,
        &anchor,
    )?;

    let new_directory = crate::manifest::encode_manifest(&manifest)?;
    if new_directory.len() != directory_bytes.len() {
        return Err("directory size changed during anchor patch".into());
    }
    image[directory_offset as usize..directory_offset as usize + new_directory.len()]
        .copy_from_slice(&new_directory);
    let new_manifest_serialized = sign_root(
        &manifest,
        root_config,
        directory_offset,
        &new_directory,
        &config.keys,
        sign,
    )?;

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

    validate_external_layout(&config.regions, &manifest, image.len())?;

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

/// Scan the image for the `FSTART_ANCHOR` placeholder at 8-byte-aligned offsets.
///
/// A valid placeholder has `FFS_MAGIC` followed by `FFS_VERSION` and then all
/// zeros (the output of `AnchorBlock::placeholder()`).  Matching only the
/// 8-byte magic would produce false positives against `FFS_MAGIC` constants
/// embedded in other binaries' `.rodata` sections.
fn scan_for_placeholders(image: &[u8]) -> Vec<usize> {
    let magic = &FFS_MAGIC;
    let mut offsets = Vec::new();
    let mut offset = 0;
    while offset + ANCHOR_SIZE <= image.len() {
        if &image[offset..offset + magic.len()] == magic {
            // Verify this is a genuine placeholder: version must match and
            // the mutable fields (manifest_offset, manifest_size,
            // total_image_size) must all be zero.
            let rest = &image[offset + magic.len()..offset + ANCHOR_SIZE];
            let version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
            let manifest_off = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
            let manifest_sz = u32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
            let total_sz = u32::from_le_bytes([rest[12], rest[13], rest[14], rest[15]]);
            if version == FFS_VERSION && manifest_off == 0 && manifest_sz == 0 && total_sz == 0 {
                offsets.push(offset);
            }
        }
        offset += 8;
    }
    offsets
}

/// Lay out a file's segments in the image, returning a `RegionEntry`.
fn lay_out_file(
    image: &mut Vec<u8>,
    file: &InputFile,
    region_base: u32,
) -> Result<RegionEntry, String> {
    let mut segments: heapless::Vec<Segment, 12> = heapless::Vec::new();
    let mut digest_input: Vec<u8> = Vec::new();

    let entry_offset = image_len(&image)? - region_base;

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
        let seg_offset = image_len(&image)? - region_base - entry_offset;

        // Append stored data to the image
        image.extend_from_slice(&stored_data);

        // Accumulate uncompressed data for whole-file digest
        digest_input.extend_from_slice(&seg.data);

        let name: HString<32> = HString::try_from(seg.name.as_str())
            .map_err(|_| format!("segment name too long: {}", seg.name))?;

        // loaded_size is total memory, initialized_size is the digest boundary.
        // A BSS tail is zeroed separately and is not part of the content hash.
        let loaded_size = seg
            .mem_size
            .map(|m| u32::try_from(m).map_err(|_| "segment memory size exceeds u32"))
            .transpose()?
            .unwrap_or(u32::try_from(seg.data.len()).map_err(|_| "segment exceeds u32")?);

        segments
            .push(Segment {
                name,
                kind: seg.kind,
                offset: seg_offset,
                stored_size: u32::try_from(stored_data.len())
                    .map_err(|_| "stored segment exceeds u32")?,
                initialized_size: u32::try_from(seg.data.len())
                    .map_err(|_| "initialized segment exceeds u32")?,
                stored_digest: segment_hash(&stored_data),
                loaded_digest: segment_hash(&seg.data),
                loaded_size,
                in_place_size,
                load_addr: seg.load_addr,
                compression: actual_compression,
                flags: seg.flags,
            })
            .map_err(|_| format!("too many segments in file '{}'", file.name))?;
    }

    // Compute digests over concatenated uncompressed segment data
    let digests = file_hash(&digest_input).map_err(|_| "no digest algorithms available")?;

    let entry_size = image_len(&image)? - region_base - entry_offset;

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

fn external_file_entry(file: &ExternalInputFile) -> Result<RegionEntry, String> {
    let mut segments: heapless::Vec<Segment, 12> = heapless::Vec::new();
    let mut digest_input: Vec<u8> = Vec::new();
    let mut entry_size = 0u32;

    for seg in &file.segments {
        if seg.compression != Compression::None {
            return Err(format!(
                "external file '{}' segment '{}' cannot be compressed",
                file.name, seg.name
            ));
        }
        let seg_offset = entry_size;
        digest_input.extend_from_slice(&seg.data);
        let loaded_size = seg
            .mem_size
            .map(|m| u32::try_from(m).map_err(|_| "segment memory size exceeds u32"))
            .transpose()?
            .unwrap_or(u32::try_from(seg.data.len()).map_err(|_| "segment exceeds u32")?);
        segments
            .push(Segment {
                name: HString::try_from(seg.name.as_str())
                    .map_err(|_| format!("segment name too long: {}", seg.name))?,
                kind: seg.kind,
                offset: seg_offset,
                stored_size: u32::try_from(seg.data.len())
                    .map_err(|_| "stored segment exceeds u32")?,
                initialized_size: u32::try_from(seg.data.len())
                    .map_err(|_| "initialized segment exceeds u32")?,
                stored_digest: segment_hash(&seg.data),
                loaded_digest: segment_hash(&seg.data),
                loaded_size,
                in_place_size: 0,
                load_addr: seg.load_addr,
                compression: Compression::None,
                flags: seg.flags,
            })
            .map_err(|_| format!("too many segments in file '{}'", file.name))?;
        entry_size = entry_size
            .checked_add(u32::try_from(seg.data.len()).map_err(|_| "external segment exceeds u32")?)
            .ok_or("external file exceeds u32")?;
    }

    let digests = file_hash(&digest_input).map_err(|_| "no digest algorithms available")?;
    let name: HString<64> = HString::try_from(file.name.as_str())
        .map_err(|_| format!("file name too long: {}", file.name))?;

    Ok(RegionEntry {
        name,
        offset: file.offset,
        size: entry_size,
        content: EntryContent::File {
            file_type: file.file_type,
            segments,
            digests,
        },
    })
}

/// Recompute the digest for whichever file contains `anchor_offset`.
///
/// After the anchor is patched, that file's on-image bytes differ from
/// the data that was hashed during layout. This function reads the actual
/// bytes from the image and updates the digest in the manifest.
fn recompute_file_digests(
    image: &[u8],
    manifest: &mut ImageManifest,
    anchor_offsets: &[usize],
    input_regions: &[InputRegion],
    anchor: &AnchorBlock,
) -> Result<(), String> {
    recompute_inline_file_digests(image, manifest, anchor_offsets)?;
    recompute_external_file_digests(manifest, input_regions, anchor)?;
    Ok(())
}

fn validate_external_layout(
    input_regions: &[InputRegion],
    manifest: &ImageManifest,
    image_len: usize,
) -> Result<(), String> {
    let image_end = u64::try_from(image_len)
        .map_err(|_| format!("FFS image length {image_len:#x} exceeds u64"))?;

    for input_region in input_regions {
        let InputRegion::ContainerWithExternal {
            name,
            external_files,
            ..
        } = input_region
        else {
            continue;
        };
        if external_files.is_empty() {
            continue;
        }

        let region = manifest
            .regions
            .iter()
            .find(|region| region.name.as_str() == name.as_str())
            .ok_or_else(|| format!("container '{name}' missing from manifest"))?;
        let region_start = u64::from(region.offset);
        let region_end = region_start
            .checked_add(u64::from(region.size))
            .ok_or_else(|| format!("container '{name}' size overflows u64"))?;
        let children = match &region.content {
            RegionContent::Container { children } => children,
            RegionContent::Raw { .. } => continue,
        };

        let mut external_ranges: Vec<(u64, u64, &str)> = Vec::new();
        for file in external_files {
            let entry = children
                .iter()
                .find(|entry| entry.name.as_str() == file.name.as_str())
                .ok_or_else(|| {
                    format!(
                        "external file '{}' missing from container '{name}'",
                        file.name
                    )
                })?;
            let start = region_start
                .checked_add(u64::from(entry.offset))
                .ok_or_else(|| format!("external file '{}' offset overflows u64", file.name))?;
            let end = start
                .checked_add(u64::from(entry.size))
                .ok_or_else(|| format!("external file '{}' size overflows u64", file.name))?;
            if end > region_end {
                return Err(format!(
                    "external file '{}' range [{start:#x}..{end:#x}) exceeds container '{name}' end {region_end:#x}",
                    file.name
                ));
            }
            if start < image_end {
                return Err(format!(
                    "FFS blob length {image_end:#x} overlaps external file '{}' at [{start:#x}..{end:#x})",
                    file.name
                ));
            }
            external_ranges.push((start, end, file.name.as_str()));
        }

        external_ranges.sort_by_key(|(start, _, _)| *start);
        for pair in external_ranges.windows(2) {
            let (_, prev_end, prev_name) = pair[0];
            let (next_start, next_end, next_name) = pair[1];
            if next_start < prev_end {
                return Err(format!(
                    "external file '{next_name}' range [{next_start:#x}..{next_end:#x}) overlaps external file '{prev_name}' ending at {prev_end:#x}"
                ));
            }
        }
    }

    Ok(())
}

fn recompute_inline_file_digests(
    image: &[u8],
    manifest: &mut ImageManifest,
    anchor_offsets: &[usize],
) -> Result<(), String> {
    for region in manifest.regions.iter_mut() {
        let region_offset = region.offset as usize;

        let children = match &mut region.content {
            RegionContent::Container { children } => children,
            _ => continue,
        };

        for entry in children.iter_mut() {
            let entry_abs = region_offset + entry.offset as usize;
            let entry_end = entry_abs + entry.size as usize;

            if !anchor_offsets
                .iter()
                .any(|&anchor_offset| anchor_offset >= entry_abs && anchor_offset < entry_end)
            {
                continue;
            }

            let (segments, digests) = match &mut entry.content {
                EntryContent::File {
                    segments, digests, ..
                } => (segments, digests),
                _ => continue,
            };

            let mut digest_input: Vec<u8> = Vec::new();
            for seg in segments.iter_mut() {
                if seg.compression != Compression::None {
                    return Err(format!(
                        "segment '{}' in file '{}' uses {:?} compression —                          files containing FSTART_ANCHOR must be uncompressed                          because anchors are patched directly into the image",
                        seg.name, entry.name, seg.compression,
                    ));
                }
                let abs_offset = region_offset + entry.offset as usize + seg.offset as usize;
                let end = abs_offset + seg.stored_size as usize;
                if end > image.len() {
                    return Err(format!(
                        "segment '{}' extends past image end during digest recomputation",
                        seg.name
                    ));
                }
                seg.stored_digest = segment_hash(&image[abs_offset..end]);
                seg.loaded_digest = seg.stored_digest;
                digest_input.extend_from_slice(&image[abs_offset..end]);
            }

            *digests = file_hash(&digest_input).map_err(|_| "no digest algorithms available")?;
        }
    }

    Ok(())
}

fn recompute_external_file_digests(
    manifest: &mut ImageManifest,
    input_regions: &[InputRegion],
    anchor: &AnchorBlock,
) -> Result<(), String> {
    for input_region in input_regions {
        let InputRegion::ContainerWithExternal {
            name,
            external_files,
            ..
        } = input_region
        else {
            continue;
        };

        let Some(region) = manifest
            .regions
            .iter_mut()
            .find(|region| region.name.as_str() == name.as_str())
        else {
            continue;
        };
        let region_offset = region.offset;
        let children = match &mut region.content {
            RegionContent::Container { children } => children,
            _ => continue,
        };

        for file in external_files {
            let Some(entry) = children
                .iter_mut()
                .find(|entry| entry.name.as_str() == file.name.as_str())
            else {
                continue;
            };
            let EntryContent::File {
                segments, digests, ..
            } = &mut entry.content
            else {
                continue;
            };

            let mut digest_input: Vec<u8> = Vec::new();
            let mut segment_base = 0u32;
            for (input_segment, segment) in file.segments.iter().zip(segments.iter_mut()) {
                if segment.compression != Compression::None {
                    return Err(format!(
                        "segment '{}' in external file '{}' uses {:?} compression — external files containing FSTART_ANCHOR must be uncompressed",
                        segment.name, entry.name, segment.compression,
                    ));
                }

                let mut data = input_segment.data.clone();
                let placeholders = scan_for_placeholders(&data);
                for placeholder_offset in placeholders {
                    let anchor_offset = region_offset
                        .checked_add(file.offset)
                        .and_then(|offset| offset.checked_add(segment_base))
                        .and_then(|offset| offset.checked_add(placeholder_offset as u32))
                        .ok_or_else(|| {
                            format!("external file '{}' anchor offset overflows u32", file.name)
                        })?;
                    let mut patched_anchor = *anchor;
                    patched_anchor.anchor_offset = anchor_offset;
                    patched_anchor.write_to(&mut data[placeholder_offset..]);
                }
                segment.stored_digest = segment_hash(&data);
                segment.loaded_digest = segment.stored_digest;
                digest_input.extend_from_slice(&data);
                segment_base = segment_base
                    .checked_add(segment.stored_size)
                    .ok_or_else(|| format!("external file '{}' size overflows u32", file.name))?;
            }

            *digests = file_hash(&digest_input).map_err(|_| "no digest algorithms available")?;
        }
    }

    Ok(())
}

/// Empty pure zero-fill segments have no content digest.
fn segment_hash(bytes: &[u8]) -> [u8; 32] {
    if bytes.is_empty() {
        [0; 32]
    } else {
        digest::hash_sha256(bytes)
    }
}
fn file_hash(bytes: &[u8]) -> Result<fstart_core::ffs::DigestSet, digest::DigestError> {
    Ok(fstart_core::ffs::DigestSet {
        sha256: Some(digest::hash_sha256(bytes)),
        sha3_256: None,
    })
}

fn sign_root<F>(
    manifest: &ImageManifest,
    config: &BootRootConfig,
    directory_offset: u32,
    directory: &[u8],
    keys: &[VerificationKey],
    sign: &F,
) -> Result<Vec<u8>, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    let mut descriptors = [None; 2];
    for (i, (name, role)) in config.bootstrap.iter().enumerate() {
        let count = manifest
            .regions
            .iter()
            .map(|r| match &r.content {
                RegionContent::Container { children } => {
                    children.iter().filter(|e| e.name.as_str() == name).count()
                }
                _ => 0,
            })
            .sum::<usize>();
        if count != 1 {
            return Err(format!("bootstrap '{name}' must identify exactly one file"));
        }
        let (region, entry) = manifest
            .regions
            .iter()
            .find_map(|r| {
                if let RegionContent::Container { children } = &r.content {
                    children
                        .iter()
                        .find(|e| e.name.as_str() == name)
                        .map(|e| (r, e))
                } else {
                    None
                }
            })
            .ok_or_else(|| format!("bootstrap file '{name}' missing"))?;
        let EntryContent::File {
            file_type: FileType::StageCode,
            segments,
            ..
        } = &entry.content
        else {
            return Err(format!("bootstrap '{name}' is not stage code"));
        };
        if segments.len() != 1 {
            return Err(format!("bootstrap '{name}' must be a flat image"));
        }
        let seg = &segments[0];
        if !seg.flags.execute || seg.initialized_size != seg.loaded_size {
            return Err(format!(
                "bootstrap '{name}' must contain initialized executable bytes only"
            ));
        }
        let descriptor = BootstrapDescriptor {
            role: *role,
            compression: seg.compression,
            offset: u64::from(region.offset) + u64::from(entry.offset) + u64::from(seg.offset),
            stored_size: u64::from(seg.stored_size),
            loaded_size: u64::from(seg.initialized_size),
            load_addr: seg.load_addr,
            entry_offset: 0,
            // Stable compressed buffer + distinct decoder destination; no overlap.
            scratch_size: if seg.compression == Compression::Lz4 {
                u64::from(seg.stored_size) + u64::from(seg.initialized_size)
            } else {
                0
            },
            stored_digest: seg.stored_digest,
            loaded_digest: seg.loaded_digest,
        };
        descriptor
            .validate(u64::MAX)
            .map_err(|e| format!("invalid bootstrap '{name}': {e:?}"))?;
        descriptors[i] = Some(descriptor);
    }
    let key = keys.first().ok_or("boot root requires an Ed25519 key")?;
    if key.signature_kind() != Some(fstart_core::ffs::SignatureKind::Ed25519) {
        return Err("boot root revision 1 requires Ed25519; ECDSA roots unsupported".into());
    }
    let mut root = Root {
        security_version: config.security_version,
        image_family: config.image_family,
        key_id: u32::from(key.key_id),
        descriptors,
        directory: DirectoryRef {
            offset: u64::from(directory_offset),
            size: directory.len() as u64,
            digest: digest::hash_sha256(directory),
        },
        signature: [0; 64],
    };
    let bytes = root.encode();
    let signature = sign(&bytes[..SIGNED_SIZE])?;
    if signature.kind != fstart_core::ffs::SignatureKind::Ed25519 || signature.key_id != key.key_id
    {
        return Err("root signer must use the selected Ed25519 key".into());
    }
    fstart_crypto::verify::verify_signature(&bytes[..SIGNED_SIZE], &signature, key)
        .map_err(|e| format!("root signer verification: {e:?}"))?;
    root.signature = signature.signature_bytes();
    Ok(root.encode().to_vec())
}

/// Reserve disjoint compressed input and decoder destination. Do not construct
/// overlapping Rust slices, even for a decoder with an overlap-aware algorithm.
fn verify_in_place_lz4(
    compressed: &[u8],
    original_size: usize,
    _seg_name: &str,
    _file_name: &str,
) -> Result<u32, String> {
    original_size
        .checked_add(compressed.len())
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| "LZ4 workspace exceeds u32".into())
}

/// Finalize a non-bootstrap, uncompressed initial stage (pins/ROM headers), then
/// rebuild its digests, the directory digest and root signature. No other bytes
/// can be changed by the callback, avoiding stale authenticated descriptors.
pub fn finalize_initial_stage<F, P>(
    built: &mut FfsImage,
    initial_name: &str,
    patch: P,
    sign: &F,
) -> Result<(), String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
    P: FnOnce(&mut [u8], &Root) -> Result<(), String>,
{
    let anchor =
        unsafe { core::ptr::read_unaligned(built.anchor_bytes.as_ptr().cast::<AnchorBlock>()) };
    let root_start = anchor.manifest_offset as usize;
    let mut root = Root::parse(
        built
            .image
            .get(root_start..root_start + ROOT_SIZE)
            .ok_or("root outside image")?,
    )
    .map_err(|e| format!("root parse: {e:?}"))?;
    let dir_start =
        usize::try_from(root.directory.offset).map_err(|_| "directory offset overflow")?;
    let dir_size = usize::try_from(root.directory.size).map_err(|_| "directory size overflow")?;
    let directory = built
        .image
        .get(
            dir_start
                ..dir_start
                    .checked_add(dir_size)
                    .ok_or("directory range overflow")?,
        )
        .ok_or("directory outside image")?;
    root.directory
        .verify_bytes(directory)
        .map_err(|e| format!("directory digest: {e:?}"))?;
    let mut manifest = crate::manifest::ManifestView::parse(directory)
        .and_then(|v| v.to_owned_manifest())
        .map_err(|e| format!("directory: {e:?}"))?;
    let (start, size) = manifest
        .regions
        .iter()
        .find_map(|region| {
            let RegionContent::Container { children } = &region.content else {
                return None;
            };
            children
                .iter()
                .find(|e| e.name.as_str() == initial_name)
                .map(|entry| (region, entry))
        })
        .ok_or("initial stage missing")
        .and_then(|(region, entry)| {
            let EntryContent::File { segments, .. } = &entry.content else {
                return Err("initial stage not a file");
            };
            if segments.len() != 1 || segments[0].compression != Compression::None {
                return Err("initial stage must be flat and uncompressed");
            }
            Ok((
                region.offset as usize + entry.offset as usize + segments[0].offset as usize,
                segments[0].stored_size as usize,
            ))
        })?;
    if root
        .descriptors
        .iter()
        .flatten()
        .any(|d| d.offset < (start + size) as u64 && d.offset + d.stored_size > start as u64)
    {
        return Err("cannot finalize a bootstrap target".into());
    }
    patch(
        built
            .image
            .get_mut(start..start + size)
            .ok_or("initial stage outside image")?,
        &root,
    )?;
    recompute_inline_file_digests(&built.image, &mut manifest, &[start])?;
    let directory = crate::manifest::encode_manifest(&manifest)?;
    if directory.len() != dir_size {
        return Err("directory size changed during finalization".into());
    }
    built.image[dir_start..dir_start + dir_size].copy_from_slice(&directory);
    root.directory.digest = digest::hash_sha256(&directory);
    let signature = sign(&root.encode()[..SIGNED_SIZE])?;
    if signature.kind != fstart_core::ffs::SignatureKind::Ed25519
        || u32::from(signature.key_id) != root.key_id
    {
        return Err("invalid root signer".into());
    }
    root.signature = signature.signature_bytes();
    built.image[root_start..root_start + ROOT_SIZE].copy_from_slice(&root.encode());
    Ok(())
}

fn image_len(image: &[u8]) -> Result<u32, String> {
    u32::try_from(image.len()).map_err(|_| "image exceeds u32 directory offsets".into())
}
