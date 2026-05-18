//! FFS image inspection — scan for the anchor and display the filesystem.
//!
//! Usage:
//!   cargo xtask inspect path/to/image.ffs

use std::fs;
use std::path::Path;

use fstart_ffs::FfsReader;
use fstart_types::ffs::{
    AnchorBlock, Compression, EntryContent, FileType, RegionContent, SegmentFlags, SegmentKind,
    SignatureKind, ANCHOR_SIZE, FFS_MAGIC, FFS_VERSION,
};

/// Read an FFS image from disk, find the anchor, and print the filesystem.
pub fn inspect(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    let data = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    println!("FFS image: {} ({} bytes)", path.display(), data.len());
    println!();

    let _full_reader = FfsReader::new(&data);

    // --- Anchor ---
    let anchors = find_anchors(&data);
    let (anchor_offset, anchor) = choose_display_anchor(&anchors)
        .ok_or_else(|| "no FFS anchor found (FSTART01 magic not present)".to_string())?;

    // Anchors store offsets relative to the FFS/BIOS image base.  Full-flash
    // images (e.g. Intel IFD pflash) embed that image at a non-zero offset, so
    // inspect the slice whose base makes the scanned anchor line up with the
    // anchor's own image-relative offset.
    let image_base = anchor_offset
        .checked_sub(anchor.anchor_offset as usize)
        .ok_or_else(|| {
            format!(
                "anchor at {anchor_offset:#x} reports larger image-relative offset {:#x}",
                anchor.anchor_offset
            )
        })?;
    let image_data = data
        .get(image_base..)
        .ok_or_else(|| format!("computed image base {image_base:#x} is outside input"))?;
    let reader = FfsReader::new(image_data);

    println!("Anchor");
    if image_base != 0 {
        println!("  image base:       {image_base:#x} ({image_base})");
    }
    println!("  offset:           {anchor_offset:#x} ({anchor_offset})");
    println!("  size:             {ANCHOR_SIZE} bytes");
    println!("  version:          {}", anchor.version);
    println!("  image size:       {} bytes", anchor.total_image_size);
    println!(
        "  anchor offset:    {:#x} ({})",
        anchor.anchor_offset, anchor.anchor_offset
    );
    if anchor.microcode_size != 0 {
        println!(
            "  microcode:        offset={:#x} size={}",
            anchor.microcode_offset, anchor.microcode_size
        );
    }
    println!(
        "  manifest offset:  {:#x} ({})",
        anchor.manifest_offset, anchor.manifest_offset
    );
    println!("  manifest size:    {} bytes", anchor.manifest_size);
    if anchor.anchor_offset >= anchor.total_image_size {
        println!("  note:             XIP anchor outside FFS blob (top-aligned XIP bootblock)");
    }
    println!("  keys:             {}", anchor.key_count);

    for (i, key) in anchor.valid_keys().iter().enumerate() {
        let alg = match key.signature_kind() {
            Some(SignatureKind::Ed25519) => "Ed25519",
            Some(SignatureKind::EcdsaP256) => "ECDSA-P256",
            None => "unknown",
        };
        println!(
            "    [{i}] id={} algorithm={alg} fingerprint={}",
            key.key_id,
            hex_short(&key.key_lo),
        );
    }
    println!();

    // --- Manifest ---
    let manifest = reader
        .read_manifest(&anchor)
        .map_err(|e| format!("failed to read/verify manifest: {e:?}"))?;

    if anchor.version != FFS_VERSION {
        println!(
            "  warning: version {} (expected {FFS_VERSION})",
            anchor.version
        );
    }
    println!(
        "Manifest (verified, {} region{})",
        manifest.regions.len(),
        if manifest.regions.len() == 1 { "" } else { "s" }
    );
    println!();

    let ro_parent = if image_base != 0 {
        manifest
            .regions
            .iter()
            .find_map(|region| match &region.content {
                RegionContent::Raw { .. }
                    if image_base >= region.offset as usize
                        && image_base < (region.offset as usize + region.size as usize) =>
                {
                    Some(region.name.as_str())
                }
                _ => None,
            })
    } else {
        None
    };

    let xip_bootblock_file_offset = if anchor.anchor_offset >= anchor.total_image_size {
        manifest.regions.iter().find_map(|region| {
            if let RegionContent::Container { children } = &region.content {
                children.iter().find_map(|entry| {
                    if entry.name.as_str() == "bootblock" {
                        let region_end = ro_parent.and_then(|parent| {
                            manifest.regions.iter().find_map(|region| {
                                if region.name.as_str() == parent {
                                    Some(region.offset as usize + region.size as usize)
                                } else {
                                    None
                                }
                            })
                        })?;
                        Some(region_end.saturating_sub(entry.size as usize))
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
    } else {
        None
    };

    // --- Regions ---
    for region in &manifest.regions {
        let kind_label = match &region.content {
            RegionContent::Container { children } => {
                format!(
                    "Container, {} entr{}",
                    children.len(),
                    if children.len() == 1 { "y" } else { "ies" }
                )
            }
            RegionContent::Raw { fill } => format!("Raw (fill={fill:#04x})"),
        };

        let region_location = match &region.content {
            RegionContent::Container { .. } if image_base != 0 => format!(
                "offset={:#x}  file_offset={:#x}",
                region.offset,
                image_base + region.offset as usize
            ),
            _ => format!("offset={:#x}", region.offset),
        };
        let display_name = if region.name.as_str() == "ro" {
            ro_parent
                .map(|parent| format!("{parent}/{}", region.name))
                .unwrap_or_else(|| region.name.to_string())
        } else {
            region.name.to_string()
        };
        println!(
            "Region \"{}\"  {}  size={} ({:#x})  [{}]",
            display_name, region_location, region.size, region.size, kind_label,
        );

        if let RegionContent::Container { children } = &region.content {
            for (i, entry) in children.iter().enumerate() {
                let is_last_entry = i == children.len() - 1;
                let branch = if is_last_entry {
                    "└── "
                } else {
                    "├── "
                };
                let cont = if is_last_entry { "    " } else { "│   " };

                match &entry.content {
                    EntryContent::File {
                        file_type,
                        segments,
                        digests,
                    } => {
                        let stored_total: u64 =
                            segments.iter().map(|seg| seg.stored_size as u64).sum();
                        let loaded_total: u64 =
                            segments.iter().map(|seg| seg.loaded_size as u64).sum();
                        let compressed_count = segments
                            .iter()
                            .filter(|seg| seg.compression != Compression::None)
                            .count();
                        let compression_summary = if compressed_count == 0 {
                            String::new()
                        } else {
                            format!(
                                "  compressed={}  decompressed={}  ratio={:.0}%",
                                stored_total,
                                loaded_total,
                                if loaded_total > 0 {
                                    stored_total as f64 / loaded_total as f64 * 100.0
                                } else {
                                    0.0
                                }
                            )
                        };

                        let entry_location = if image_base != 0 {
                            format!(
                                "offset={:#x}  file_offset={:#x}",
                                entry.offset,
                                image_base + region.offset as usize + entry.offset as usize
                            )
                        } else {
                            format!("offset={:#x}", entry.offset)
                        };
                        let bootblock_note = if entry.name.as_str() == "bootblock" {
                            let entry_file_offset =
                                image_base + region.offset as usize + entry.offset as usize;
                            xip_bootblock_file_offset
                                .map(|off| {
                                    if off == entry_file_offset {
                                        "  note=XIP top-aligned".to_string()
                                    } else {
                                        format!(
                                            "  note=FFS copy; XIP top-aligned at file_offset={off:#x}"
                                        )
                                    }
                                })
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        println!(
                            "  {branch}\"{}\"  type={}  {}  size={}{}{}",
                            entry.name,
                            file_type_str(*file_type),
                            entry_location,
                            entry.size,
                            compression_summary,
                            bootblock_note,
                        );

                        // Digests
                        if let Some(h) = &digests.sha256 {
                            println!("  {cont}    sha256:  {}", hex_full(h));
                        }
                        if let Some(h) = &digests.sha3_256 {
                            println!("  {cont}    sha3:    {}", hex_full(h));
                        }

                        // Segments
                        for (j, seg) in segments.iter().enumerate() {
                            let is_last_seg = j == segments.len() - 1;
                            let seg_branch = if is_last_seg {
                                "└── "
                            } else {
                                "├── "
                            };

                            let comp = match seg.compression {
                                Compression::None => String::new(),
                                Compression::Lz4 => format!(
                                    "  compression=LZ4 ratio={:.0}%",
                                    if seg.loaded_size > 0 {
                                        seg.stored_size as f64 / seg.loaded_size as f64 * 100.0
                                    } else {
                                        0.0
                                    }
                                ),
                            };

                            let kind = match seg.kind {
                                SegmentKind::Code => "code",
                                SegmentKind::ReadOnlyData => "rodata",
                                SegmentKind::ReadWriteData => "data",
                                SegmentKind::Bss => "bss",
                            };

                            let flags = flags_str(seg.flags);

                            println!(
                                "  {cont}    {seg_branch}{} ({kind})  load={:#x}  \
                                 stored={}  loaded={}  flags=[{flags}]{comp}",
                                seg.name, seg.load_addr, seg.stored_size, seg.loaded_size,
                            );

                            if seg.in_place_size > 0 {
                                let seg_cont = if is_last_seg { "    " } else { "│   " };
                                println!(
                                    "  {cont}    {seg_cont}  in-place buffer: {} bytes",
                                    seg.in_place_size
                                );
                            }
                        }
                    }
                    EntryContent::Raw { fill } => {
                        let entry_location = if image_base != 0 {
                            format!(
                                "offset={:#x}  file_offset={:#x}",
                                entry.offset,
                                image_base + region.offset as usize + entry.offset as usize
                            )
                        } else {
                            format!("offset={:#x}", entry.offset)
                        };
                        println!(
                            "  {branch}\"{}\"  type=Raw  {}  size={}  fill={fill:#04x}",
                            entry.name, entry_location, entry.size,
                        );
                    }
                }
            }
        }

        println!();
    }

    Ok(())
}

fn find_anchors(data: &[u8]) -> Vec<(usize, AnchorBlock)> {
    let reader = FfsReader::new(data);
    let mut anchors = Vec::new();
    let mut offset = 0usize;
    while offset + ANCHOR_SIZE <= data.len() {
        if data[offset..offset + FFS_MAGIC.len()] == FFS_MAGIC {
            if let Ok(anchor) = reader.read_anchor(offset) {
                anchors.push((offset, anchor));
            }
        }
        offset += 8;
    }
    anchors
}

fn choose_display_anchor(anchors: &[(usize, AnchorBlock)]) -> Option<(usize, AnchorBlock)> {
    // Full-flash x86 images contain both the FFS copy of the anchor and the
    // XIP bootblock's top-of-flash anchor. Prefer the XIP anchor when present;
    // it has the same image base but an image-relative offset beyond the FFS
    // blob's signed total_image_size.
    anchors
        .iter()
        .copied()
        .find(|(_, anchor)| anchor.anchor_offset >= anchor.total_image_size)
        .or_else(|| anchors.first().copied())
}

fn file_type_str(ft: FileType) -> &'static str {
    match ft {
        FileType::StageCode => "StageCode",
        FileType::BoardConfig => "BoardConfig",
        FileType::Payload => "Payload",
        FileType::Fdt => "Fdt",
        FileType::Data => "Data",
        FileType::Raw => "Raw",
        FileType::Firmware => "Firmware",
        FileType::FitImage => "FitImage",
        FileType::CpuMicrocode => "CpuMicrocode",
        FileType::Initramfs => "Initramfs",
    }
}

fn flags_str(f: SegmentFlags) -> String {
    let mut s = String::new();
    if f.read {
        s.push('r');
    }
    if f.write {
        s.push('w');
    }
    if f.execute {
        s.push('x');
    }
    if s.is_empty() {
        s.push('-');
    }
    s
}

/// First 8 bytes as hex with trailing ellipsis.
fn hex_short(bytes: &[u8; 32]) -> String {
    let prefix: String = bytes[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}...")
}

/// Full 32-byte digest as hex string.
fn hex_full(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
