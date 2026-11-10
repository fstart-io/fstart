use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "layout_linker_tests.rs"]
mod layout_linker_tests;

pub struct BuildResult {
    pub stages: Vec<fstart_image_build::StageBinary>,
    /// Every unit output, including SMM images and generated ABI headers.
    pub artifacts: std::collections::BTreeMap<String, std::collections::BTreeMap<String, PathBuf>>,
}

impl BuildResult {
    pub fn primary_binary(&self) -> &fstart_image_build::StageBinary {
        &self.stages[0]
    }
}

pub fn build_with_parsed(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    parsed: &crate::build_plan::ParsedBoard,
    release: bool,
) -> Result<BuildResult, String> {
    let resolved = parsed
        .resolved
        .as_ref()
        .ok_or_else(|| "board has no resolved platform plan".to_string())?;
    resolved.build(workspace_root, board_manifest, release)
}

pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

pub(crate) fn write_flat_binary(elf_path: &Path, bin_path: &Path) -> Result<(), String> {
    let elf_data = fs::read(elf_path)
        .map_err(|e| format!("failed to read ELF {}: {e}", elf_path.display()))?;
    let load_segments = elf_load_segments(&elf_data, elf_path)?;

    let min_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr)
        .min()
        .ok_or_else(|| format!("no loadable file-backed segments in {}", elf_path.display()))?;
    let max_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr.saturating_add(segment.filesz))
        .max()
        .unwrap_or(min_paddr);
    let len = max_paddr
        .checked_sub(min_paddr)
        .and_then(|len| usize::try_from(len).ok())
        .ok_or_else(|| {
            format!(
                "flat binary address range is too large in {}",
                elf_path.display()
            )
        })?;

    let mut flat = vec![0u8; len];
    for segment in load_segments.iter().filter(|segment| segment.filesz != 0) {
        let start = usize::try_from(segment.paddr - min_paddr)
            .map_err(|_| format!("segment offset is too large in {}", elf_path.display()))?;
        let size = usize::try_from(segment.filesz)
            .map_err(|_| format!("segment size is too large in {}", elf_path.display()))?;
        let file_start = usize::try_from(segment.offset)
            .map_err(|_| format!("segment file offset is too large in {}", elf_path.display()))?;
        let file_end = file_start
            .checked_add(size)
            .ok_or_else(|| format!("segment file range overflows in {}", elf_path.display()))?;
        if file_end > elf_data.len() {
            return Err(format!(
                "PT_LOAD at {:#x} extends past EOF in {}",
                segment.paddr,
                elf_path.display()
            ));
        }
        flat[start..start + size].copy_from_slice(&elf_data[file_start..file_end]);
    }

    fs::write(bin_path, flat)
        .map_err(|e| format!("failed to write flat binary {}: {e}", bin_path.display()))
}

#[derive(Debug, Clone, Copy)]
struct ElfLoadSegment {
    offset: u64,
    paddr: u64,
    filesz: u64,
}

fn elf_load_segments(elf_data: &[u8], elf_path: &Path) -> Result<Vec<ElfLoadSegment>, String> {
    if elf_data.len() < 16 {
        return Err(format!("ELF {} is too short", elf_path.display()));
    }

    match elf_data[4] {
        elf::ELFCLASS32 => elf_load_segments_for::<elf::FileHeader32<object::Endianness>>(elf_data),
        elf::ELFCLASS64 => elf_load_segments_for::<elf::FileHeader64<object::Endianness>>(elf_data),
        class => {
            return Err(format!(
                "unsupported ELF class {class} in {}",
                elf_path.display()
            ));
        }
    }
    .map_err(|e| format!("failed to parse ELF {}: {e}", elf_path.display()))
}

fn elf_load_segments_for<Elf>(elf_data: &[u8]) -> Result<Vec<ElfLoadSegment>, object::Error>
where
    Elf: FileHeader,
{
    let elf = ElfFile::<Elf>::parse(elf_data)?;
    let endian = elf.elf_header().endian()?;
    Ok(elf
        .elf_program_headers()
        .iter()
        .filter(|phdr| phdr.p_type(endian) == elf::PT_LOAD)
        .map(|phdr| ElfLoadSegment {
            offset: phdr.p_offset(endian).into(),
            paddr: phdr.p_paddr(endian).into(),
            filesz: phdr.p_filesz(endian).into(),
        })
        .collect())
}

fn workspace_root() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var("FSTART_WORKSPACE_ROOT") {
        return Ok(PathBuf::from(root));
    }

    let mut dir = std::env::current_dir().map_err(|e| format!("no cwd: {e}"))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root".to_string());
        }
    }
}
