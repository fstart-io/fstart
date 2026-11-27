// SPDX-License-Identifier: MIT OR Apache-2.0
//! Intel Firmware Interface Table, NOT U-Boot FIT. Host-only generation over
//! FFS-owned, uncompressed RO bytes. Single reset bank, header + microcode only.
//! No ACM/KM/BPM, top-swap provisioning or Boot Guard authorization is implied.
use fstart_core::{
    ffs::{Compression, FileType, SegmentFlags, SegmentKind},
    services::FirmwareImage,
};
use fstart_ffs::builder::{FileLayout, FilePatch, InputFile, InputSegment};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LE, U16, U64},
};

pub const NAME: &str = "intel_fit";
pub const MICROCODE: &str = "cpu_microcode_blob.bin";
pub const POINTER: u64 = 0xffff_ffc0;

pub fn validate_pointer_section(elf: &[u8]) -> Result<(), String> {
    use object::{Object, ObjectSection};
    let elf = object::File::parse(elf).map_err(|e| format!("FIT reset ELF: {e}"))?;
    let section = elf
        .section_by_name(".intel_fit_pointer")
        .ok_or("reset ELF has no reserved Intel FIT pointer section")?;
    if section.address() != POINTER
        || section.size() != 8
        || section.data().map_err(|_| "FIT pointer section data")? != [0xff; 8]
    {
        return Err("malformed Intel FIT pointer reservation".into());
    }
    Ok(())
}
/// 24-bit unsigned little-endian integer (Intel FIT 3-byte size field).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(transparent)]
struct U24Le([u8; 3]);

impl U24Le {
    const fn new(value: u32) -> Self {
        let b = value.to_le_bytes();
        Self([b[0], b[1], b[2]])
    }

    const fn get(self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], 0])
    }
}

const SIGNATURE: u64 = u64::from_le_bytes(*b"_FIT_   ");
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Entry {
    address: U64<LE>,
    size: U24Le,
    reserved: u8,
    version: U16<LE>,
    kind: u8,
    checksum: u8,
}
const _: () = assert!(core::mem::size_of::<Entry>() == 16);
fn entry(address: u64, count: usize, kind: u8) -> Entry {
    Entry {
        address: address.into(),
        size: U24Le::new(count as u32),
        reserved: 0,
        version: 0x100.into(),
        kind,
        checksum: 0,
    }
}
fn unique<'a>(files: &'a [FileLayout], name: &str) -> Result<&'a FileLayout, String> {
    let mut matches = files.iter().filter(|f| f.file == name);
    let file = matches
        .next()
        .ok_or_else(|| format!("missing flat RO FIT source '{name}'"))?;
    if matches.next().is_some() {
        return Err(format!("ambiguous FIT source '{name}'"));
    }
    Ok(file)
}
#[derive(Clone)]
pub struct Plan {
    count: usize,
    digest: [u8; 32],
}
impl Plan {
    pub fn prepare(files: &mut Vec<InputFile>) -> Result<Self, String> {
        if files.iter().any(|f| f.name == NAME) {
            return Err("reserved Intel FIT name already supplied".into());
        }
        let mut matches = files.iter().filter(|f| f.name == MICROCODE);
        let file = matches.next().ok_or("Intel FIT requires microcode")?;
        if matches.next().is_some()
            || file.file_type != FileType::CpuMicrocode
            || file.segments.len() != 1
            || file.segments[0].compression != Compression::None
        {
            return Err("FIT requires one uncompressed microcode file".into());
        }
        let data = &file.segments[0].data;
        let count = fstart_core::intel_microcode::records(data).try_fold(0usize, |n, record| {
            record
                .map(|_| n + 1)
                .map_err(|e| format!("invalid Intel microcode: {e:?}"))
        })?;
        if count == 0 || count >= 0xffffff {
            return Err("invalid FIT entry count".into());
        }
        let digest = fstart_crypto::digest::hash_sha256(data);
        files.push(InputFile {
            name: NAME.into(),
            file_type: FileType::Data,
            segments: vec![InputSegment {
                name: ".fit".into(),
                kind: SegmentKind::ReadOnlyData,
                data: vec![0xff; (count + 1) * 16],
                mem_size: None,
                load_addr: 0,
                compression: Compression::None,
                flags: SegmentFlags::RODATA,
            }],
        });
        Ok(Self { count, digest })
    }
    pub fn resolve(
        &self,
        files: &[FileLayout],
        image: FirmwareImage,
        initial: &str,
    ) -> Result<(Vec<FilePatch>, Resolved), String> {
        image
            .validate()
            .map_err(|_| "invalid FIT firmware mapping")?;
        let window = image
            .contiguous_window()
            .filter(|w| w.cpu_end() == Some(0x1_0000_0000))
            .ok_or("FIT requires one top-of-4G BIOS window; top-swap unsupported")?;
        let table = unique(files, NAME)?;
        let microcode = unique(files, MICROCODE)?;
        let initial = unique(files, initial)?;
        if table.external
            || microcode.external
            || table.file_type != FileType::Data
            || microcode.file_type != FileType::CpuMicrocode
            || !initial.external
            || initial.file_type != FileType::StageCode
            || files.iter().filter(|f| f.external).count() != 1
        {
            return Err(
                "FIT requires inline RO table/microcode and one external reset image".into(),
            );
        }
        for file in [table, microcode, initial] {
            if u64::from(file.offset)
                .checked_add(file.data.len() as u64)
                .is_none_or(|end| end > image.size)
            {
                return Err("FIT file outside BIOS window".into());
            }
        }
        let table_address = image
            .translate(table.offset.into())
            .ok_or("FIT address outside mapping")?;
        let code_address = image
            .translate(microcode.offset.into())
            .ok_or("microcode outside mapping")?;
        let initial_address = image
            .translate(initial.offset.into())
            .ok_or("reset image outside mapping")?;
        if initial.load_addr != initial_address
            || initial_address + initial.data.len() as u64 != 0x1_0000_0000
            || table_address & 15 != 0
            || code_address & 15 != 0
        {
            return Err("FIT alignment/reset placement mismatch".into());
        }
        if fstart_crypto::digest::hash_sha256(&microcode.data) != self.digest {
            return Err("FIT microcode changed after inventory".into());
        }
        let records = fstart_core::intel_microcode::records(&microcode.data)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("microcode: {e:?}"))?;
        if records.len() != self.count || table.data.len() != (self.count + 1) * 16 {
            return Err("FIT size changed after layout".into());
        }
        let mut bytes = entry(SIGNATURE, self.count + 1, 0x80).as_bytes().to_vec();
        for record in records {
            bytes.extend_from_slice(entry(code_address + record.offset as u64, 0, 1).as_bytes());
        }
        bytes[15] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        let pointer_offset = POINTER
            .checked_sub(initial_address)
            .ok_or("FIT pointer outside reset image")? as usize;
        if initial.data.get(pointer_offset..pointer_offset + 8) != Some(&[0xff; 8]) {
            return Err("FIT pointer slot is not the reserved erased placeholder".into());
        }
        let patches = vec![
            FilePatch {
                file: NAME.into(),
                offset: 0,
                expected: table.data.clone(),
                bytes: bytes.clone(),
            },
            FilePatch {
                file: initial.file.clone(),
                offset: pointer_offset,
                expected: vec![0xff; 8],
                bytes: table_address.to_le_bytes().to_vec(),
            },
        ];
        Ok((
            patches,
            Resolved {
                base: window.cpu_base,
                size: window.size,
                table_offset: table.offset as usize,
                table: bytes,
                microcode_offset: microcode.offset as usize,
                microcode_size: microcode.data.len(),
                digest: self.digest,
            },
        ))
    }
}

pub struct Resolved {
    base: u64,
    size: u64,
    table_offset: usize,
    table: Vec<u8>,
    microcode_offset: usize,
    microcode_size: usize,
    digest: [u8; 32],
}
impl Resolved {
    /// Audit the actual emitted BIOS bytes, not the compiler ELF or draft.
    pub fn verify(&self, bios: &[u8]) -> Result<(), String> {
        if bios.len() as u64 != self.size {
            return Err("FIT audit BIOS size mismatch".into());
        }
        let report = inspect_ffs(bios, self.base)?;
        if report.table_address != self.base + self.table_offset as u64
            || bios.get(self.table_offset..self.table_offset + self.table.len())
                != Some(self.table.as_slice())
        {
            return Err("emitted FIT differs from finalized FFS file".into());
        }
        let microcode = bios
            .get(self.microcode_offset..self.microcode_offset + self.microcode_size)
            .ok_or("emitted microcode outside BIOS")?;
        if fstart_crypto::digest::hash_sha256(microcode) != self.digest {
            return Err("emitted microcode differs from FFS source".into());
        }
        let expected = fstart_core::intel_microcode::records(microcode)
            .map(|r| r.map(|r| self.base + self.microcode_offset as u64 + r.offset as u64))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("microcode: {e:?}"))?;
        if report.microcode != expected {
            return Err("FIT entries differ from FFS inventory".into());
        }
        Ok(())
    }
}
/// Resolve the emitted index back through the actual root/directory and
/// verify the indexed file digests. As elsewhere in host inspection, the media
/// policy is not protected key authority or a deployment authorization.
pub fn inspect_ffs(bios: &[u8], base: u64) -> Result<Report, String> {
    use fstart_core::ffs::{EntryContent, RegionContent};
    let report = inspect(bios, base)?;
    let reader = fstart_ffs::FfsReader::new(bios);
    let locator = reader
        .read_anchor(
            reader
                .scan_for_anchor()
                .map_err(|e| format!("FIT locator: {e:?}"))?,
        )
        .map_err(|e| format!("FIT locator: {e:?}"))?;
    let manifest = reader
        .read_manifest(&locator)
        .map_err(|e| format!("FIT directory: {e:?}"))?;
    let source = |name: &str, expected: FileType| -> Result<(usize, &[u8]), String> {
        let mut found = None;
        for region in &manifest.regions {
            let RegionContent::Container { children } = &region.content else {
                continue;
            };
            for file in children {
                if file.name.as_str() != name {
                    continue;
                }
                let EntryContent::File {
                    file_type,
                    segments,
                    ..
                } = &file.content
                else {
                    return Err("FIT source is not a file".into());
                };
                if found.is_some() || *file_type != expected || segments.len() != 1 {
                    return Err("ambiguous/invalid FIT source".into());
                }
                let segment = &segments[0];
                if segment.compression != Compression::None
                    || !segment.flags.read
                    || segment.flags.write
                    || segment.stored_size != segment.initialized_size
                    || segment.loaded_size != segment.initialized_size
                {
                    return Err("FIT source is not flat read-only data".into());
                }
                let data = reader
                    .read_segment_data(segment, region, file)
                    .map_err(|e| format!("FIT source bytes: {e:?}"))?;
                reader
                    .verify_entry_digests(file, region)
                    .map_err(|e| format!("FIT source digest: {e:?}"))?;
                if fstart_crypto::digest::hash_sha256(data) != segment.stored_digest
                    || segment.loaded_digest != segment.stored_digest
                {
                    return Err("FIT segment digest mismatch".into());
                }
                let offset = region
                    .offset
                    .checked_add(file.offset)
                    .and_then(|v| v.checked_add(segment.offset))
                    .ok_or("FIT source offset overflow")? as usize;
                found = Some((offset, data));
            }
        }
        found.ok_or_else(|| format!("FIT source '{name}' missing from directory"))
    };
    // The fixed pointer must be inside one signed, executable XIP reset file,
    // not an unsigned gap. Verify that entire file, including its locators.
    let pointer = POINTER.checked_sub(base).ok_or("FIT pointer below BIOS")?;
    let mut reset_owner = None;
    for region in &manifest.regions {
        let RegionContent::Container { children } = &region.content else {
            continue;
        };
        for file in children {
            let EntryContent::File {
                file_type,
                segments,
                ..
            } = &file.content
            else {
                continue;
            };
            if segments.len() != 1 {
                continue;
            }
            let segment = &segments[0];
            let start = region
                .offset
                .checked_add(file.offset)
                .and_then(|v| v.checked_add(segment.offset))
                .ok_or("FIT reset offset overflow")? as u64;
            let end = start + u64::from(segment.stored_size);
            if start <= pointer && pointer + 8 <= end {
                if reset_owner.is_some()
                    || *file_type != FileType::StageCode
                    || !segment.flags.execute
                    || segment.load_addr != base + start
                    || end != bios.len() as u64
                {
                    return Err("FIT pointer has no unique XIP reset owner".into());
                }
                reset_owner = Some(file.name.as_str());
            }
        }
    }
    source(
        reset_owner.ok_or("FIT pointer is outside signed reset bytes")?,
        FileType::StageCode,
    )?;
    let (table_offset, table) = source(NAME, FileType::Data)?;
    let (code_offset, code) = source(MICROCODE, FileType::CpuMicrocode)?;
    let addresses = fstart_core::intel_microcode::records(code)
        .map(|record| record.map(|record| base + code_offset as u64 + record.offset as u64))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("FIT source records: {e:?}"))?;
    if addresses.is_empty()
        || report.microcode != addresses
        || report.table_address != base + table_offset as u64
        || table.len() != (addresses.len() + 1) * 16
        || locator.microcode_offset.get() as usize != code_offset
        || locator.microcode_size.get() as usize != code.len()
    {
        return Err("FIT/locator/directory source mismatch".into());
    }
    Ok(report)
}

#[derive(Debug)]
pub struct Report {
    pub table_address: u64,
    pub microcode: Vec<u64>,
}
/// Decode the supported header/microcode subset. Checksums are not signatures.
pub fn inspect(bios: &[u8], base: u64) -> Result<Report, String> {
    if base.checked_add(bios.len() as u64) != Some(0x1_0000_0000) {
        return Err("FIT audit needs top-aligned BIOS bytes".into());
    }
    let pointer = POINTER
        .checked_sub(base)
        .ok_or("FIT pointer outside BIOS")? as usize;
    let address = U64::<LE>::read_from_bytes(
        bios.get(pointer..pointer + 8)
            .ok_or("FIT pointer truncated")?,
    )
    .map_err(|_| "FIT pointer")?
    .get();
    if address & 15 != 0 {
        return Err("unaligned FIT pointer".into());
    }
    let offset = usize::try_from(address.checked_sub(base).ok_or("FIT below BIOS")?)
        .map_err(|_| "FIT offset")?;
    let (header, _) = Entry::read_from_prefix(bios.get(offset..).ok_or("FIT outside BIOS")?)
        .map_err(|_| "FIT header truncated")?;
    let count = header.size.get() as usize;
    if header.address.get() != SIGNATURE
        || header.kind != 0x80
        || header.version.get() != 0x100
        || header.reserved != 0
        || count == 0
    {
        return Err("invalid FIT header".into());
    }
    let end = offset
        .checked_add(count.checked_mul(16).ok_or("FIT size overflow")?)
        .ok_or("FIT end overflow")?;
    let table = bios.get(offset..end).ok_or("FIT table outside BIOS")?;
    if offset < pointer + 8 && pointer < end {
        return Err("FIT table overlaps pointer".into());
    }
    if table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return Err("FIT checksum".into());
    }
    let mut microcode = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for bytes in table[16..].as_chunks::<16>().0 {
        let entry = Entry::read_from_bytes(bytes).map_err(|_| "FIT entry")?;
        if entry.kind != 1
            || entry.size.get() != 0
            || entry.reserved != 0
            || entry.version.get() != 0x100
            || entry.checksum != 0
        {
            return Err("unsupported/invalid FIT entry".into());
        }
        let address = entry.address.get();
        let index = usize::try_from(address.checked_sub(base).ok_or("microcode below BIOS")?)
            .map_err(|_| "microcode offset")?;
        if address & 15 != 0 || microcode.contains(&address) {
            return Err("duplicate/unaligned FIT microcode".into());
        }
        let record = fstart_core::intel_microcode::records(
            bios.get(index..).ok_or("FIT microcode outside BIOS")?,
        )
        .next()
        .ok_or("empty FIT microcode")?
        .map_err(|e| format!("FIT microcode: {e:?}"))?;
        if index < end && offset < index + record.bytes().len()
            || index < pointer + 8 && pointer < index + record.bytes().len()
        {
            return Err("FIT microcode overlaps table/pointer".into());
        }
        let record_end = index + record.bytes().len();
        if spans
            .iter()
            .any(|&(start, end)| index < end && start < record_end)
        {
            return Err("overlapping FIT microcode records".into());
        }
        spans.push((index, record_end));
        microcode.push(address);
    }
    Ok(Report {
        table_address: address,
        microcode,
    })
}
