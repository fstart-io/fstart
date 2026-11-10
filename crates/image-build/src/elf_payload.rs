//! Package a bare ELF payload: its loadable segments flattened into one FFS
//! payload segment, plus a verified asset with the entry point.

use fstart_core::ffs::{FileType, SegmentFlags, SegmentKind};
use fstart_core::payload_manifest::{ELF_MANIFEST_ASSET, ElfManifest};
use fstart_ffs::builder::{InputFile, InputSegment};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn add(
    payload: &fstart_core::PayloadConfig,
    board_dir: &Path,
    elf_path: Option<&str>,
    ro_files: &mut Vec<InputFile>,
) -> Result<(), String> {
    let elf_path = elf_path
        .map(PathBuf::from)
        .or_else(|| {
            payload
                .kernel_file
                .as_ref()
                .map(|f| board_dir.join(f.as_str()))
        })
        .ok_or("ELF payload needs its ELF (--kernel)")?;
    let data = fs::read(&elf_path)
        .map_err(|e| format!("failed to read ELF payload {}: {e}", elf_path.display()))?;
    let image = crate::elf_flat::flatten(&data)
        .map_err(|e| format!("ELF payload {}: {e}", elf_path.display()))?;
    let manifest = ElfManifest { entry: image.entry };
    ro_files.push(crate::elf_flat::payload_file(image, payload.compression));
    ro_files.push(InputFile {
        name: ELF_MANIFEST_ASSET.into(),
        file_type: FileType::Data,
        segments: vec![InputSegment {
            name: ".data".into(),
            kind: SegmentKind::ReadOnlyData,
            data: manifest.encode().to_vec(),
            mem_size: None,
            load_addr: 0,
            compression: fstart_core::ffs::Compression::None,
            flags: SegmentFlags::RODATA,
        }],
    });
    Ok(())
}
