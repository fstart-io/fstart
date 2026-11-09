//! Package a coreboot payload: its ELF, plus one asset with the launch
//! policy and the files handed to it.
//!
//! The ELF's loadable segments are flattened into one FFS payload segment,
//! which the runtime loads like any other verified file. The payload never
//! reads flash.

use fstart_core::ffs::{Compression, FileType, SegmentFlags, SegmentKind};
use fstart_coreboot::manifest;
use fstart_ffs::builder::{InputFile, InputSegment};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn add(
    payload: &fstart_core::PayloadConfig,
    board_dir: &Path,
    elf_path: Option<&str>,
    files: &[(String, String)],
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
        .ok_or("coreboot payload needs its ELF (--kernel)")?;
    let data = fs::read(&elf_path).map_err(|e| {
        format!(
            "failed to read coreboot payload {}: {e}",
            elf_path.display()
        )
    })?;
    let image = crate::elf_flat::flatten(&data)
        .map_err(|e| format!("coreboot payload {}: {e}", elf_path.display()))?;

    ro_files.push(InputFile {
        name: "payload".into(),
        file_type: FileType::Payload,
        segments: vec![InputSegment {
            name: ".text".into(),
            kind: SegmentKind::Code,
            mem_size: (image.mem_size > image.data.len() as u64).then_some(image.mem_size),
            data: image.data,
            load_addr: image.load_addr,
            compression: payload.compression,
            flags: SegmentFlags::CODE,
        }],
    });

    let contents = files
        .iter()
        .map(|(name, path)| {
            fs::read(path)
                .map(|data| (name.as_str(), data))
                .map_err(|e| format!("failed to read payload file {path}: {e}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let files: Vec<(&str, &[u8])> = contents.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    let mut bytes = vec![0; manifest::encoded_len(&files)];
    manifest::encode(image.entry, &files, &mut bytes)?;
    ro_files.push(data_file(
        manifest::ASSET.into(),
        bytes,
        payload.compression,
    ));
    Ok(())
}

fn data_file(name: String, data: Vec<u8>, compression: Compression) -> InputFile {
    InputFile {
        name,
        file_type: FileType::Data,
        segments: vec![InputSegment {
            name: ".data".into(),
            kind: SegmentKind::ReadOnlyData,
            data,
            mem_size: None,
            load_addr: 0,
            compression,
            flags: SegmentFlags::RODATA,
        }],
    }
}
