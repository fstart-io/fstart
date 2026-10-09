//! Package a coreboot payload: its ELF, plus one asset with the launch
//! policy and the files handed to it.
//!
//! The ELF's loadable segments are flattened into one FFS payload segment,
//! which the runtime loads like any other verified file. The payload never
//! reads flash.

use fstart_core::ffs::{Compression, FileType, SegmentFlags, SegmentKind};
use fstart_coreboot::manifest;
use fstart_ffs::builder::{InputFile, InputSegment};
use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::fs;
use std::path::{Path, PathBuf};

/// Largest flattened payload: guards against ELFs with far-apart segments.
const MAX_SPAN: u64 = 16 << 20;

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
    let image =
        flatten(&data).map_err(|e| format!("coreboot payload {}: {e}", elf_path.display()))?;

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

/// An ELF's loadable segments as one contiguous image.
struct FlatImage {
    load_addr: u64,
    data: Vec<u8>,
    /// Loaded footprint including trailing BSS.
    mem_size: u64,
    entry: u64,
}

fn flatten(elf_data: &[u8]) -> Result<FlatImage, String> {
    match elf_data.get(4) {
        Some(&elf::ELFCLASS32) => flatten_for::<elf::FileHeader32<object::Endianness>>(elf_data),
        Some(&elf::ELFCLASS64) => flatten_for::<elf::FileHeader64<object::Endianness>>(elf_data),
        _ => Err("not an ELF file".into()),
    }
}

fn flatten_for<Elf: FileHeader>(elf_data: &[u8]) -> Result<FlatImage, String> {
    let elf = ElfFile::<Elf>::parse(elf_data).map_err(|e| e.to_string())?;
    let endian = elf.elf_header().endian().map_err(|e| e.to_string())?;
    let segments = elf
        .elf_program_headers()
        .iter()
        .filter(|phdr| phdr.p_type(endian) == elf::PT_LOAD && phdr.p_memsz(endian).into() != 0)
        .map(|phdr| {
            let paddr: u64 = phdr.p_paddr(endian).into();
            let file = phdr
                .data(endian, elf_data)
                .map_err(|_| "segment outside the file")?;
            Ok((paddr, file, phdr.p_memsz(endian).into()))
        })
        .collect::<Result<Vec<(u64, &[u8], u64)>, &str>>()?;
    let start = segments
        .iter()
        .map(|s| s.0)
        .min()
        .ok_or("no loadable segments")?;
    let end = segments
        .iter()
        .map(|&(paddr, _, memsz)| paddr.checked_add(memsz).ok_or("segment wraps"))
        .try_fold(start, |end, segment| segment.map(|s| end.max(s)))?;
    let initialized_end = segments
        .iter()
        .map(|&(paddr, file, _)| paddr + file.len() as u64)
        .max()
        .unwrap_or(start);
    if end - start > MAX_SPAN || end > 1 << 32 {
        return Err(format!(
            "segments [{start:#x}..{end:#x}) are not one compact image below 4 GiB"
        ));
    }
    let mut data = vec![0; (initialized_end - start) as usize];
    for (paddr, file, _) in &segments {
        let at = (paddr - start) as usize;
        data[at..at + file.len()].copy_from_slice(file);
    }
    let entry = elf.elf_header().e_entry(endian).into();
    if !(start..end).contains(&entry) {
        return Err(format!("entry {entry:#x} lies outside the loaded image"));
    }
    Ok(FlatImage {
        load_addr: start,
        data,
        mem_size: end - start,
        entry,
    })
}
