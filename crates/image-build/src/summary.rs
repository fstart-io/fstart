//! End-of-build image layout, in the spirit of coreboot printing
//! `cbfstool print` once a build finishes. `fbuild inspect` keeps the full
//! per-segment detail; this is the one table worth reading after a build.

use std::fmt::Write;

use fstart_core::ffs::{Compression, EntryContent, FileType, ImageManifest, Region, RegionContent};

/// A top-level area of the written output file, in file offsets.
pub(crate) struct FlashArea {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

impl FlashArea {
    pub fn new(name: impl Into<String>, offset: impl Into<u64>, size: impl Into<u64>) -> Self {
        Self {
            name: name.into(),
            offset: offset.into(),
            size: size.into(),
        }
    }
}

/// Where the FFS image landed in the written output file.
pub(crate) struct Placement {
    /// Areas of a composite flash image; empty for a bare FFS image.
    pub areas: Vec<FlashArea>,
    /// File offset of the FFS image.
    pub ffs_offset: u64,
}

impl Placement {
    pub fn bare() -> Self {
        Self {
            areas: Vec::new(),
            ffs_offset: 0,
        }
    }
}

/// Render `manifest` of an `ffs_size`-byte FFS image written into the
/// `file_size`-byte output `name` as described by `placement`.
///
/// Every row carries two offsets: into the written file (the flash offset
/// of a composite image) and into the FFS image. Flash areas outside the
/// FFS image have no FFS offset.
pub(crate) fn render(
    name: &str,
    file_size: u64,
    ffs_size: u64,
    manifest: &ImageManifest,
    placement: &Placement,
) -> String {
    let mut table = Table::new(placement.ffs_offset);
    for area in placement.areas.iter().filter(|area| area.size != 0) {
        let ffs = area.offset.checked_sub(placement.ffs_offset);
        table.row(&area.name, area.offset, ffs, "area", area.size, "");
    }

    // Raw regions named after a flash area (Intel descriptor, ME, ...) are
    // recorded in the manifest in flash coordinates; the areas cover them.
    let is_area = |region: &Region| {
        matches!(region.content, RegionContent::Raw { .. })
            && placement
                .areas
                .iter()
                .any(|a| a.name == region.name.as_str())
    };
    // Bytes inside the FFS blob that no file owns hold its trust block,
    // directory and anchor; past the blob the flash is still erased.
    let gap = |table: &mut Table, indent: &str, from: u64, to: u64| {
        for (label, a, b) in [
            ("(metadata)", from, to.min(ffs_size).max(from)),
            ("(empty)", from.max(ffs_size).min(to), to),
        ] {
            if b > a {
                table.ffs_row(&format!("{indent}{label}"), a, "", b - a, "");
            }
        }
    };
    let mut regions_end = 0;
    for region in manifest.regions.iter().filter(|region| !is_area(region)) {
        let base = u64::from(region.offset);
        let end = base + u64::from(region.size);
        regions_end = regions_end.max(end);
        let RegionContent::Container { children } = &region.content else {
            table.ffs_row(&region.name, base, "raw", region.size.into(), "");
            continue;
        };
        table.ffs_row(&region.name, base, "region", region.size.into(), "");
        let mut entries: Vec<_> = children.iter().collect();
        entries.sort_by_key(|entry| entry.offset);
        let mut cursor = base;
        for entry in entries {
            let start = base + u64::from(entry.offset);
            gap(&mut table, "  ", cursor, start);
            let (kind, comp) = match &entry.content {
                EntryContent::File {
                    file_type,
                    segments,
                    ..
                } => {
                    let loaded: u64 = segments.iter().map(|s| u64::from(s.loaded_size)).sum();
                    let comp = if segments.iter().any(|s| s.compression == Compression::Lz4) {
                        format!("LZ4 ({loaded} decompressed)")
                    } else {
                        "none".into()
                    };
                    (file_type_name(*file_type), comp)
                }
                EntryContent::Raw { .. } => ("raw", "none".into()),
            };
            table.ffs_row(
                &format!("  {}", entry.name),
                start,
                kind,
                entry.size.into(),
                &comp,
            );
            cursor = cursor.max(start + u64::from(entry.size));
        }
        gap(&mut table, "  ", cursor, end);
    }
    // Images without a top-aligned region keep their metadata after the
    // last region rather than inside one.
    gap(&mut table, "", regions_end, ffs_size);

    format!("{name}: {}\n{}", human(file_size), table.out)
}

struct Table {
    ffs_offset: u64,
    out: String,
}

impl Table {
    fn new(ffs_offset: u64) -> Self {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<28} {:>10} {:>10}  {:<9} {:>10}  Comp",
            "Name", "File off", "FFS off", "Type", "Size"
        );
        Self { ffs_offset, out }
    }

    fn row(&mut self, name: &str, file: u64, ffs: Option<u64>, kind: &str, size: u64, comp: &str) {
        let ffs = ffs.map_or_else(|| "-".into(), |offset| format!("{offset:#x}"));
        let line = format!("{name:<28} {file:>#10x} {ffs:>10}  {kind:<9} {size:>10}  {comp}");
        let _ = writeln!(self.out, "{}", line.trim_end());
    }

    fn ffs_row(&mut self, name: &str, ffs: u64, kind: &str, size: u64, comp: &str) {
        self.row(name, self.ffs_offset + ffs, Some(ffs), kind, size, comp);
    }
}

fn file_type_name(file_type: FileType) -> &'static str {
    match file_type {
        FileType::StageCode => "stage",
        FileType::BoardConfig => "config",
        FileType::Payload => "payload",
        FileType::Fdt => "fdt",
        FileType::Data => "data",
        FileType::Raw => "raw",
        FileType::Firmware => "firmware",
        FileType::FitImage => "fit",
        FileType::CpuMicrocode => "microcode",
        FileType::Initramfs => "initramfs",
    }
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 && b % (1 << 20) == 0 => format!("{} MiB", b >> 20),
        b if b >= 1 << 10 && b % (1 << 10) == 0 => format!("{} KiB", b >> 10),
        b => format!("{b} B"),
    }
}
