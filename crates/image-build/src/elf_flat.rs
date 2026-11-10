//! Flatten an ELF payload's loadable segments into one image: gaps between
//! segments are zero-filled, trailing BSS stays a memory size the loader
//! clears.

use fstart_core::ffs::{Compression, FileType, SegmentFlags, SegmentKind};
use fstart_ffs::builder::{InputFile, InputSegment};
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

/// The FFS payload file loading `image` at its address.
pub(crate) fn payload_file(image: FlatImage, compression: Compression) -> InputFile {
    InputFile {
        name: "payload".into(),
        file_type: FileType::Payload,
        segments: vec![InputSegment {
            name: ".text".into(),
            kind: SegmentKind::Code,
            mem_size: (image.mem_size > image.data.len() as u64).then_some(image.mem_size),
            data: image.data,
            load_addr: image.load_addr,
            compression,
            flags: SegmentFlags::CODE,
        }],
    }
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
            let memsz: u64 = phdr.p_memsz(endian).into();
            if file.len() as u64 > memsz {
                return Err("segment has more file data than memory");
            }
            Ok((paddr, file, memsz))
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

#[cfg(test)]
mod tests {
    use super::flatten;

    /// A little-endian ELF32 executable with one `PT_LOAD` segment of
    /// `data` at `paddr`, `memsz` bytes in memory, entered at `paddr`.
    fn elf32(paddr: u32, data: &[u8], memsz: u32) -> Vec<u8> {
        let mut elf = vec![0x7F, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        for half in [2u16, 40] {
            elf.extend(half.to_le_bytes());
        }
        for word in [1u32, paddr, 52, 0, 0] {
            elf.extend(word.to_le_bytes());
        }
        for half in [52u16, 32, 1, 40, 0, 0] {
            elf.extend(half.to_le_bytes());
        }
        let filesz = data.len() as u32;
        for word in [1u32, 84, paddr, paddr, filesz, memsz, 5, 4] {
            elf.extend(word.to_le_bytes());
        }
        elf.extend(data);
        elf
    }

    #[test]
    fn trailing_memory_is_bss() {
        let image = flatten(&elf32(0x8000_0000, &[1, 2, 3, 4], 16)).unwrap();
        assert_eq!(
            (image.load_addr, image.data, image.mem_size, image.entry),
            (0x8000_0000, vec![1, 2, 3, 4], 16, 0x8000_0000)
        );
    }

    #[test]
    fn segments_with_more_file_than_memory_are_rejected() {
        assert!(flatten(&elf32(0x8000_0000, &[1, 2, 3, 4, 5, 6, 7, 8], 4)).is_err());
    }
}
