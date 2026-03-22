//! FFS image inspection — scan for the anchor and display the filesystem.
//!
//! Usage:
//!   cargo xtask inspect path/to/image.ffs

use std::fs;
use std::path::Path;

use fstart_ffs::FfsReader;
use fstart_types::ffs::{
    Compression, EntryContent, FileType, RegionContent, SegmentFlags, SegmentKind, SignatureKind,
    ANCHOR_SIZE, FFS_VERSION,
};

/// Read an FFS image from disk, find the anchor, and print the filesystem.
pub fn inspect(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    let data = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    println!("FFS image: {} ({} bytes)", path.display(), data.len());
    println!();

    let reader = FfsReader::new(&data);

    // --- Anchor ---
    let anchor_offset = reader
        .scan_for_anchor()
        .map_err(|_| "no FFS anchor found (FSTART01 magic not present)".to_string())?;

    let anchor = reader
        .read_anchor(anchor_offset)
        .map_err(|e| format!("failed to read anchor at offset {anchor_offset:#x}: {e:?}"))?;

    println!("Anchor");
    println!("  offset:           {anchor_offset:#x} ({anchor_offset})");
    println!("  size:             {ANCHOR_SIZE} bytes");
    println!("  version:          {}", anchor.version);
    println!("  image size:       {} bytes", anchor.total_image_size);
    println!(
        "  manifest offset:  {:#x} ({})",
        anchor.manifest_offset, anchor.manifest_offset
    );
    println!("  manifest size:    {} bytes", anchor.manifest_size);
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

        println!(
            "Region \"{}\"  offset={:#x}  size={} ({:#x})  [{}]",
            region.name, region.offset, region.size, region.size, kind_label,
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
                        println!(
                            "  {branch}\"{}\"  type={}  offset={:#x}  size={}",
                            entry.name,
                            file_type_str(*file_type),
                            entry.offset,
                            entry.size,
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
                        println!(
                            "  {branch}\"{}\"  type=Raw  offset={:#x}  size={}  fill={fill:#04x}",
                            entry.name, entry.offset, entry.size,
                        );
                    }
                }
            }
        }

        println!();
    }

    Ok(())
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
