//! Flatten an ELF payload's loadable segments into one image: gaps between
//! segments are zero-filled, trailing BSS stays a memory size the loader
//! clears.

use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};

/// Largest flattened payload: guards against ELFs with far-apart segments.
const MAX_SPAN: u64 = 16 << 20;

/// An ELF's loadable segments as one contiguous image.
pub(crate) struct FlatImage {
    pub load_addr: u64,
    pub data: Vec<u8>,
    /// Loaded footprint including trailing BSS.
    pub mem_size: u64,
    pub entry: u64,
}

pub(crate) fn flatten(elf_data: &[u8]) -> Result<FlatImage, String> {
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
