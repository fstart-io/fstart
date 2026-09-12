//! Architecture-neutral linked-image checks driven by concrete expectations.
use crate::plan::Span;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "elf_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Architecture {
    Arm,
    Aarch64,
    Riscv64,
    X86_64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyMapping {
    pub storage: Span,
    pub execution: Span,
    pub end_symbol: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descriptor {
    pub section: String,
    pub start_symbol: String,
    pub end_symbol: String,
    pub bytes: Vec<u8>,
    pub reservation: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expectations {
    pub architecture: Architecture,
    pub elf64: bool,
    pub little_endian: bool,
    pub stored: Vec<Span>,
    pub runtime: Vec<Span>,
    pub identity_mapping: bool,
    pub entry: Option<u64>,
    pub copy: Option<CopyMapping>,
    pub descriptor: Descriptor,
    pub symbols: BTreeMap<String, u64>,
}

/// Check an address extent without confusing its exclusive end with an address.
/// A 32-bit range may end at 2^32; entry points and symbol values may not.
pub(crate) fn address_extent(elf64: bool, base: u64, size: u64) -> Result<(), String> {
    let end = base.checked_add(size).ok_or("address extent overflow")?;
    if !elf64 && (base > u64::from(u32::MAX) || end > (1u64 << 32)) {
        return Err("address extent is not representable in 32-bit ELF".into());
    }
    Ok(())
}

impl Expectations {
    pub fn validate(&self) -> Result<(), String> {
        for range in self
            .stored
            .iter()
            .chain(&self.runtime)
            .chain([&self.descriptor.reservation])
            .chain(
                self.copy
                    .iter()
                    .flat_map(|copy| [&copy.storage, &copy.execution]),
            )
        {
            range.end()?;
            address_extent(self.elf64, range.base, range.size)?;
        }
        for address in self.entry.iter().chain(self.symbols.values()) {
            address_extent(self.elf64, *address, 0)?;
        }
        let layout = fstart_core::layout::Layout::parse(&self.descriptor.bytes)
            .map_err(|e| e.to_string())?;
        for region in layout.regions() {
            address_extent(self.elf64, region.base, region.size)?;
        }
        Ok(())
    }
}

fn segments<Elf: FileHeader>(
    elf: &ElfFile<'_, Elf>,
    expected: &Expectations,
) -> Result<u64, String> {
    let mut count = 0;
    let mut stored_end = 0;
    for segment in elf
        .elf_program_headers()
        .iter()
        .filter(|s| s.p_type(elf.endian()) == object::elf::PT_LOAD)
    {
        let paddr = segment.p_paddr(elf.endian()).into();
        let vaddr = segment.p_vaddr(elf.endian()).into();
        let filesz = segment.p_filesz(elf.endian()).into();
        let memsz = segment.p_memsz(elf.endian()).into();
        address_extent(expected.elf64, paddr, filesz)?;
        address_extent(expected.elf64, vaddr, memsz)?;
        if filesz > memsz
            || (filesz != 0 && !expected.stored.iter().any(|r| r.contains(paddr, filesz)))
            || (memsz != 0 && !expected.runtime.iter().any(|r| r.contains(vaddr, memsz)))
            || (memsz != 0 && expected.identity_mapping && paddr != vaddr)
        {
            return Err(format!(
                "PT_LOAD exceeds physical/virtual expectations: {paddr:#x}/{vaddr:#x}, {filesz:#x}/{memsz:#x}"
            ));
        }
        if memsz != 0 {
            count += 1;
        }
        if filesz != 0 {
            stored_end = stored_end.max(paddr.checked_add(filesz).ok_or("PT_LOAD overflow")?);
            if let Some(copy) = &expected.copy
                && copy.execution.contains(vaddr, memsz)
                && vaddr - copy.execution.base
                    != paddr
                        .checked_sub(copy.storage.base)
                        .ok_or("copy storage underflow")?
            {
                return Err("execution PT_LOAD is not a linear copy of image storage".into());
            }
        }
    }
    if count == 0 {
        return Err("ELF has no load segments".into());
    }
    Ok(stored_end)
}

pub fn validate(bytes: &[u8], expected: &Expectations) -> Result<(), String> {
    expected.validate()?;
    let file = object::File::parse(bytes).map_err(|e| e.to_string())?;
    let architecture = match expected.architecture {
        Architecture::Arm => object::Architecture::Arm,
        Architecture::Aarch64 => object::Architecture::Aarch64,
        Architecture::Riscv64 => object::Architecture::Riscv64,
        Architecture::X86_64 => object::Architecture::X86_64,
    };
    if file.architecture() != architecture || file.is_little_endian() != expected.little_endian {
        return Err("ELF architecture/endianness mismatch".into());
    }
    let stored_end = match &file {
        object::File::Elf32(elf) if !expected.elf64 => segments(elf, expected)?,
        object::File::Elf64(elf) if expected.elf64 => segments(elf, expected)?,
        _ => return Err("ELF class mismatch".into()),
    };
    address_extent(expected.elf64, file.entry(), 0)?;
    for symbol in file.symbols() {
        address_extent(expected.elf64, symbol.address(), symbol.size())?;
    }
    let has_symbol = |name: &str, address| {
        file.symbols()
            .any(|s| s.name() == Ok(name) && s.address() == address)
    };
    if expected.entry.is_some_and(|entry| file.entry() != entry) {
        return Err("ELF entry mismatch".into());
    }
    if let Some(copy) = &expected.copy {
        let size = stored_end
            .checked_sub(copy.storage.base)
            .ok_or("copy extent underflow")?;
        if size > copy.execution.size
            || !has_symbol(
                &copy.end_symbol,
                copy.execution
                    .base
                    .checked_add(size)
                    .ok_or("copy extent overflow")?,
            )
        {
            return Err("relocation copy extent differs from stored PT_LOAD bytes or exceeds execution capacity".into());
        }
    }
    let descriptor = &expected.descriptor;
    let section = file
        .section_by_name(&descriptor.section)
        .ok_or("missing layout descriptor")?;
    if section.data().map_err(|e| e.to_string())? != descriptor.bytes
        || !descriptor
            .reservation
            .contains(section.address(), section.size())
    {
        return Err("descriptor differs from resolved expectations".into());
    }
    let (section_offset, section_length) =
        section.file_range().ok_or("descriptor has no file bytes")?;
    if !file.segments().any(|segment| {
        let (offset, length) = segment.file_range();
        Span {
            base: offset,
            size: length,
        }
        .contains(section_offset, section_length)
            && section.address().checked_sub(segment.address())
                == section_offset.checked_sub(offset)
    }) {
        return Err("descriptor file bytes are not covered by its loaded virtual mapping".into());
    }
    let object::SectionFlags::Elf { sh_flags } = section.flags() else {
        return Err("non-ELF descriptor".into());
    };
    if section.address() % 8 != 0
        || section.file_range().is_none()
        || sh_flags & u64::from(object::elf::SHF_ALLOC) == 0
        || sh_flags & u64::from(object::elf::SHF_WRITE) != 0
    {
        return Err("descriptor is not loaded aligned read-only data".into());
    }
    for (name, address) in expected
        .symbols
        .iter()
        .map(|(n, a)| (n.as_str(), *a))
        .chain([
            (descriptor.start_symbol.as_str(), section.address()),
            (
                descriptor.end_symbol.as_str(),
                section
                    .address()
                    .checked_add(section.size())
                    .ok_or("descriptor overflow")?,
            ),
        ])
    {
        if !has_symbol(name, address) {
            return Err(format!("ELF symbol {name} differs from expectation"));
        }
    }
    Ok(())
}
