//! FFS builder — constructs firmware images (std only, used by xtask).
//!
//! The builder produces a complete firmware image:
//!
//! 1. Collects files and their segments.
//! 2. Computes digests for each file.
//! 3. Lays out regions (RO, optionally RW-A, RW-B, NVS).
//! 4. Builds manifests for each region.
//! 5. Serializes and signs the manifests.
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
    AnchorBlock, Compression, FileEntry, FileType, Manifest, NvsPointer, RegionRole, RwSlotPointer,
    Segment, SegmentFlags, SegmentKind, Signature, SignedManifest, VerificationKey, FFS_MAGIC,
    FFS_VERSION,
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
    /// Raw uncompressed data (empty for BSS).
    pub data: Vec<u8>,
    /// Load address.
    pub load_addr: u64,
    /// Compression to apply.
    pub compression: Compression,
    /// Memory flags.
    pub flags: SegmentFlags,
}

/// Configuration for a region in the FFS image.
pub struct RegionConfig {
    /// Role of this region.
    pub role: RegionRole,
    /// Files in this region.
    pub files: Vec<InputFile>,
}

/// Configuration for building an FFS image.
pub struct FfsImageConfig {
    /// Verification keys to embed in the anchor.
    pub keys: Vec<VerificationKey>,
    /// The RO region — always present.
    pub ro_region: RegionConfig,
    /// Optional RW regions (0, 1, or 2).
    pub rw_regions: Vec<RegionConfig>,
    /// Optional NVS region size (bytes). If Some, reserved space is added.
    pub nvs_size: Option<u32>,
}

/// Result of building an FFS image.
pub struct FfsImage {
    /// The complete firmware image bytes.
    pub image: Vec<u8>,
    /// Offset of the anchor block in the image (for patching into bootblock).
    pub anchor_offset: usize,
    /// Serialized anchor block bytes (for embedding in bootblock static).
    pub anchor_bytes: Vec<u8>,
    /// Base offset of the RO region's file data in the image.
    ///
    /// Pass this to `FfsReader::read_segment_data()` as `region_base` when
    /// reading segments from the RO manifest.
    pub ro_region_base: u32,
}

/// Build an FFS image from the given configuration.
///
/// `sign` is called to sign each manifest (RO and RW). It receives the
/// raw manifest bytes and must return a `Signature`.
pub fn build_image<F>(config: &FfsImageConfig, sign: &F) -> Result<FfsImage, String>
where
    F: Fn(&[u8]) -> Result<Signature, String>,
{
    let mut image: Vec<u8> = Vec::new();

    // ---- Phase 1: Reserve space for the anchor block ----
    // We'll patch the anchor at the end once we know all offsets.
    // For now, reserve a generous amount. We'll use offset 0 for the anchor.
    let anchor_placeholder_size = 512; // enough for anchor + 4 keys
    image.resize(anchor_placeholder_size, 0xFF);

    // ---- Phase 2: Build RO region ----
    let ro_region_base = image.len() as u32;
    let mut ro_entries: heapless::Vec<FileEntry, 32> = heapless::Vec::new();

    for file in &config.ro_region.files {
        let entry = lay_out_file(&mut image, file, ro_region_base)?;
        ro_entries
            .push(entry)
            .map_err(|_| "too many RO files (max 32)".to_string())?;
    }

    // ---- Phase 3: Build RW regions ----
    let mut rw_slot_pointers: heapless::Vec<RwSlotPointer, 2> = heapless::Vec::new();

    // We need to lay out RW files, then come back and write their manifests.
    // Store intermediate state per RW region.
    struct RwRegionState {
        role: RegionRole,
        region_base: u32,
        manifest_offset: u32,
        manifest_size: u32,
        region_size: u32,
    }

    let mut rw_states: Vec<RwRegionState> = Vec::new();

    for rw_config in &config.rw_regions {
        let region_base = image.len() as u32;
        let mut entries: heapless::Vec<FileEntry, 32> = heapless::Vec::new();

        for file in &rw_config.files {
            let entry = lay_out_file(&mut image, file, region_base)?;
            entries
                .push(entry)
                .map_err(|_| "too many RW files (max 32)".to_string())?;
        }

        // Build RW manifest
        let manifest = Manifest {
            region: rw_config.role,
            entries,
            rw_slots: heapless::Vec::new(), // RW manifests don't point to further RW slots
            nvs: None,
        };

        let signed = sign_manifest(&manifest, sign)?;
        let manifest_offset = image.len() as u32;
        let manifest_bytes =
            postcard::to_allocvec(&signed).map_err(|e| format!("serialize RW manifest: {e}"))?;
        let manifest_size = manifest_bytes.len() as u32;
        image.extend_from_slice(&manifest_bytes);

        let region_size = image.len() as u32 - region_base;

        rw_states.push(RwRegionState {
            role: rw_config.role,
            region_base,
            manifest_offset,
            manifest_size,
            region_size,
        });
    }

    // Build RW slot pointers for the RO manifest
    for state in &rw_states {
        rw_slot_pointers
            .push(RwSlotPointer {
                role: state.role,
                manifest_offset: state.manifest_offset,
                manifest_size: state.manifest_size,
                region_base: state.region_base,
                region_size: state.region_size,
            })
            .map_err(|_| "too many RW slots (max 2)".to_string())?;
    }

    // ---- Phase 4: NVS region ----
    let nvs = if let Some(nvs_size) = config.nvs_size {
        let offset = image.len() as u32;
        // Fill NVS with 0xFF (erased flash)
        image.resize(image.len() + nvs_size as usize, 0xFF);
        Some(NvsPointer {
            offset,
            size: nvs_size,
        })
    } else {
        None
    };

    // ---- Phase 5: Build and sign RO manifest ----
    let ro_manifest = Manifest {
        region: RegionRole::Ro,
        entries: ro_entries,
        rw_slots: rw_slot_pointers,
        nvs,
    };

    let ro_signed = sign_manifest(&ro_manifest, sign)?;
    let ro_manifest_offset = image.len() as u32;
    let ro_manifest_serialized =
        postcard::to_allocvec(&ro_signed).map_err(|e| format!("serialize RO manifest: {e}"))?;
    let ro_manifest_size = ro_manifest_serialized.len() as u32;
    image.extend_from_slice(&ro_manifest_serialized);

    // ---- Phase 6: Build anchor block ----
    let total_image_size = image.len() as u32;

    let mut keys_heapless: heapless::Vec<VerificationKey, 4> = heapless::Vec::new();
    for key in &config.keys {
        keys_heapless
            .push(key.clone())
            .map_err(|_| "too many keys (max 4)".to_string())?;
    }

    let anchor = AnchorBlock {
        magic: FFS_MAGIC,
        version: FFS_VERSION,
        ro_manifest_offset,
        ro_manifest_size,
        total_image_size,
        ro_region_base,
        keys: keys_heapless,
    };

    let anchor_bytes =
        postcard::to_allocvec(&anchor).map_err(|e| format!("serialize anchor: {e}"))?;

    if anchor_bytes.len() > anchor_placeholder_size {
        return Err(format!(
            "anchor block ({} bytes) exceeds reserved space ({anchor_placeholder_size} bytes)",
            anchor_bytes.len()
        ));
    }

    // Patch the anchor into the reserved space at offset 0
    image[..anchor_bytes.len()].copy_from_slice(&anchor_bytes);

    Ok(FfsImage {
        image,
        anchor_offset: 0,
        anchor_bytes,
        ro_region_base,
    })
}

/// Lay out a file's segments in the image, returning a `FileEntry`.
fn lay_out_file(
    image: &mut Vec<u8>,
    file: &InputFile,
    region_base: u32,
) -> Result<FileEntry, String> {
    let mut segments: heapless::Vec<Segment, 8> = heapless::Vec::new();
    let mut digest_input: Vec<u8> = Vec::new();

    for seg in &file.segments {
        let stored_data = match seg.compression {
            Compression::None => seg.data.clone(),
            Compression::Lz4 => {
                // LZ4 compression is not yet implemented. Fail explicitly
                // rather than storing uncompressed data with an Lz4 metadata
                // tag, which would produce a corrupt image.
                return Err(format!(
                    "LZ4 compression requested for segment '{}' in file '{}' \
                     but not yet implemented",
                    seg.name, file.name
                ));
            }
        };

        // Record the offset relative to the region base
        let offset = image.len() as u32 - region_base;

        // Append stored data to the image
        image.extend_from_slice(&stored_data);

        // Accumulate uncompressed data for whole-file digest
        digest_input.extend_from_slice(&seg.data);

        let name: HString<32> = HString::try_from(seg.name.as_str())
            .map_err(|_| format!("segment name too long: {}", seg.name))?;

        segments
            .push(Segment {
                name,
                kind: seg.kind,
                offset,
                stored_size: stored_data.len() as u32,
                loaded_size: seg.data.len() as u32,
                load_addr: seg.load_addr,
                compression: seg.compression,
                flags: seg.flags,
            })
            .map_err(|_| format!("too many segments in file '{}'", file.name))?;
    }

    // Compute digests over concatenated uncompressed segment data
    let digests =
        digest::hash_digest_set(&digest_input).map_err(|_| "no digest algorithms available")?;

    let name: HString<64> = HString::try_from(file.name.as_str())
        .map_err(|_| format!("file name too long: {}", file.name))?;

    Ok(FileEntry {
        name,
        file_type: file.file_type,
        segments,
        digests,
    })
}

/// Serialize a manifest, sign it, and return a `SignedManifest`.
fn sign_manifest<F>(manifest: &Manifest, sign: &F) -> Result<SignedManifest, String>
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
