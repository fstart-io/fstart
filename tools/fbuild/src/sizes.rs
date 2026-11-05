//! Stage size records for CI's size report (`fbuild build --size-report`).
//!
//! One JSON line per board build, appended to the report file:
//! `{"board": "...", "stages": {"<stage>": {"text": .., "rodata": ..,
//! "data": .., "bss": .., "flat": ..}}}`. `flat` is the loaded image
//! (`stage.bin`); `bss` leaves out the heap, stack and page-table
//! reservations, which are layout policy rather than code.

use object::elf::{SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE};
use object::{Object, ObjectSection, SectionFlags, SectionKind};
use std::io::Write;
use std::path::Path;

/// NOLOAD reservations the linker scripts size from the stage layout.
const RESERVATIONS: &[&str] = &[".fstart.heap", ".stack", ".page_tables", ".sp3.page_tables"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StageSizes {
    pub text: u64,
    pub rodata: u64,
    pub data: u64,
    pub bss: u64,
    pub flat: u64,
}

/// Sizes of a stage ELF by section kind, plus its flat image `flat`.
pub fn stage_sizes(elf: &[u8], flat: u64) -> Result<StageSizes, String> {
    let file = object::File::parse(elf).map_err(|e| format!("stage ELF: {e}"))?;
    let mut sizes = StageSizes {
        flat,
        ..StageSizes::default()
    };
    for section in file.sections() {
        let SectionFlags::Elf { sh_flags } = section.flags() else {
            continue;
        };
        if sh_flags & u64::from(SHF_ALLOC) == 0 {
            continue;
        }
        let name = section.name().unwrap_or("");
        let size = section.size();
        let nobits = matches!(
            section.kind(),
            SectionKind::UninitializedData | SectionKind::UninitializedTls
        );
        if sh_flags & u64::from(SHF_EXECINSTR) != 0 {
            sizes.text += size;
        } else if nobits {
            if !RESERVATIONS.contains(&name) {
                sizes.bss += size;
            }
        } else if name.starts_with(".rodata") || sh_flags & u64::from(SHF_WRITE) == 0 {
            sizes.rodata += size;
        } else {
            sizes.data += size;
        }
    }
    Ok(sizes)
}

/// Append `board`'s stage sizes to the JSON-lines report at `path`.
pub fn append(
    path: &Path,
    board: &str,
    stages: &[fstart_image_build::StageBinary],
) -> Result<(), String> {
    let mut record = serde_json::Map::new();
    for stage in stages {
        let elf = std::fs::read(&stage.path)
            .map_err(|e| format!("read {}: {e}", stage.path.display()))?;
        let flat = std::fs::metadata(&stage.run_path)
            .map_err(|e| format!("stat {}: {e}", stage.run_path.display()))?
            .len();
        let s = stage_sizes(&elf, flat)?;
        record.insert(
            stage.name.clone(),
            serde_json::json!({
                "text": s.text, "rodata": s.rodata, "data": s.data,
                "bss": s.bss, "flat": s.flat,
            }),
        );
    }
    let line = serde_json::json!({ "board": board, "stages": record });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use object::write::{Object as WriteObject, StandardSection};
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    #[test]
    fn sections_are_classified_and_reservations_excluded() {
        // One of each section class, plus every reservation (SP3's 1 MiB of
        // page tables among them), which must not count as bss.
        let mut elf = WriteObject::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
        let text = elf.section_id(StandardSection::Text);
        elf.append_section_data(text, &[0x90; 100], 16);
        let rodata = elf.section_id(StandardSection::ReadOnlyData);
        elf.append_section_data(rodata, &[1; 40], 8);
        let data = elf.section_id(StandardSection::Data);
        elf.append_section_data(data, &[2; 12], 8);
        let bss = elf.section_id(StandardSection::UninitializedData);
        elf.append_section_bss(bss, 64, 8);
        for reservation in super::RESERVATIONS {
            let section = elf.add_section(
                Vec::new(),
                reservation.as_bytes().to_vec(),
                SectionKind::UninitializedData,
            );
            elf.append_section_bss(section, 0x10_0000, 4096);
        }
        let bytes = elf.write().unwrap();
        let sizes = super::stage_sizes(&bytes, 4096).unwrap();
        assert_eq!(
            sizes,
            super::StageSizes {
                text: 100,
                rodata: 40,
                data: 12,
                bss: 64,
                flat: 4096,
            }
        );
    }
}
