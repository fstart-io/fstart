use std::path::{Path, PathBuf};
use std::process::Command;

use object::{Object, ObjectSection, ObjectSymbol, RelocationKind, SectionFlags};

use fstart_smm::header::{
    CorebootOffsets, EntryDescriptor, FLAG_COREBOOT_HEADER, FLAG_COREBOOT_MODULE_ARGS,
    SmmImageHeader, render_coreboot_header,
};
#[cfg(test)]
use fstart_smm::runtime::SmmEntryParams;
use fstart_smm::runtime::{CorebootModuleArgs, MAX_SMM_CPUS, SmmRuntime};

#[cfg(not(rust_analyzer))]
mod asm {
    include!(concat!(env!("OUT_DIR"), "/smm_image_asm.rs"));
}

#[cfg(rust_analyzer)]
mod asm {
    pub const ENTRY_STUB: &[u8] = &[];
    pub const ENTRY_PARAMS_OFFSET: usize = 0;
}

#[derive(Debug)]
pub enum BuildError {
    NoEntries,
    TooManyEntries,
    BadStackSize,
    Overflow,
    Io(std::io::Error),
    Tool(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEntries => write!(f, "SMM image must contain at least one entry point"),
            Self::TooManyEntries => write!(
                f,
                "SMM image entry count exceeds ABI maximum ({MAX_SMM_CPUS})"
            ),
            Self::BadStackSize => write!(f, "SMM stack size must be non-zero"),
            Self::Overflow => write!(f, "SMM image layout arithmetic overflowed"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Tool(e) => write!(f, "SMM stage build failed: {e}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<std::io::Error> for BuildError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageOptions {
    pub entry_count: u16,
    pub stack_size: u32,
    pub coreboot_module_args: bool,
    pub coreboot_header: bool,
}

#[derive(Debug, Clone)]
pub struct BuiltImage {
    pub image: Vec<u8>,
    pub coreboot_header: Option<String>,
}

pub fn build_image(
    options: ImageOptions,
    handler: &SmmHandlerImage,
) -> Result<BuiltImage, BuildError> {
    validate_options(options)?;

    let stub = asm::ENTRY_STUB;
    let common_code = handler.code.as_slice();

    let header_size = size_of::<SmmImageHeader>();
    let desc_size = size_of::<EntryDescriptor>();
    let params_offset = asm::ENTRY_PARAMS_OFFSET;
    let stub_size = stub.len();
    let common_runtime_offset = align_up(common_code.len(), 16)?;
    let mut common_size = common_runtime_offset
        .checked_add(size_of::<SmmRuntime>())
        .ok_or(BuildError::Overflow)?;

    let module_args_offset = if options.coreboot_module_args {
        let off = align_up(common_size, 16)?;
        let size = size_of::<CorebootModuleArgs>()
            .checked_mul(options.entry_count as usize)
            .ok_or(BuildError::Overflow)?;
        common_size = off.checked_add(size).ok_or(BuildError::Overflow)?;
        off
    } else {
        0
    };

    let entries_offset = header_size;
    let common_offset = align_up(
        entries_offset
            .checked_add(desc_size * options.entry_count as usize)
            .ok_or(BuildError::Overflow)?,
        16,
    )?;
    let stubs_offset = align_up(
        common_offset
            .checked_add(common_size)
            .ok_or(BuildError::Overflow)?,
        16,
    )?;
    let image_size = stubs_offset
        .checked_add(stub_size * options.entry_count as usize)
        .ok_or(BuildError::Overflow)?;

    let mut flags = 0;
    if options.coreboot_module_args {
        flags |= FLAG_COREBOOT_MODULE_ARGS;
    }
    if options.coreboot_header {
        flags |= FLAG_COREBOOT_HEADER;
    }

    let header = SmmImageHeader::new(
        flags,
        as_u32(image_size)?,
        options.entry_count,
        as_u32(entries_offset)?,
        as_u32(common_offset)?,
        as_u32(common_size)?,
        as_u32(handler.entry_offset)?,
        as_u32(common_runtime_offset)?,
        if options.coreboot_module_args {
            as_u32(common_offset + module_args_offset)?
        } else {
            0
        },
        if options.coreboot_module_args {
            as_u32(size_of::<CorebootModuleArgs>() * options.entry_count as usize)?
        } else {
            0
        },
        options.stack_size,
    );

    let mut image = vec![0u8; image_size];
    put_header(&mut image, 0, &header);

    for i in 0..options.entry_count as usize {
        let desc_off = entries_offset + i * desc_size;
        let stub_off = stubs_offset + i * stub_size;
        put_entry_descriptor(
            &mut image,
            desc_off,
            &EntryDescriptor {
                stub_offset: as_u32(stub_off)?,
                stub_size: as_u32(stub_size)?,
                entry_offset: 0,
                params_offset: as_u32(params_offset)?,
            },
        );
        image[stub_off..stub_off + stub_size].copy_from_slice(stub);
    }

    image[common_offset..common_offset + common_code.len()].copy_from_slice(common_code);

    let coreboot_header = options.coreboot_header.then(|| {
        render_coreboot_header(
            CorebootOffsets {
                native_header: 0,
                entries: entries_offset as u32,
                common: common_offset as u32,
                common_entry: handler.entry_offset as u32,
                runtime: common_runtime_offset as u32,
                module_args: header.module_args_offset,
                entry_count: options.entry_count,
            },
            desc_size as u16,
        )
    });

    Ok(BuiltImage {
        image,
        coreboot_header,
    })
}

pub fn write_image(
    options: ImageOptions,
    handler: &SmmHandlerImage,
    image_path: &Path,
    header_path: Option<&Path>,
) -> Result<BuiltImage, BuildError> {
    let built = build_image(options, handler)?;
    if let Some(parent) = image_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(image_path, &built.image)?;

    if let Some(path) = header_path {
        let header = built.coreboot_header.as_deref().unwrap_or("");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, header)?;
    }

    Ok(built)
}

#[derive(Debug, Clone)]
pub struct SmmHandlerImage {
    pub code: Vec<u8>,
    pub entry_offset: usize,
}

pub fn handler_from_archive(
    archive: &Path,
    work_dir: &Path,
) -> Result<SmmHandlerImage, BuildError> {
    std::fs::create_dir_all(work_dir)?;
    let elf = work_dir.join("smm_handler.elf");

    run_tool(
        Command::new("ld")
            .arg("-nostdlib")
            .arg("-Ttext=0")
            .arg("--oformat=elf64-x86-64")
            .arg("-o")
            .arg(&elf)
            .arg("--whole-archive")
            .arg(archive)
            .arg("--no-whole-archive"),
    )?;
    handler_from_elf(&elf, work_dir)
}

pub fn handler_from_elf(elf: &Path, work_dir: &Path) -> Result<SmmHandlerImage, BuildError> {
    std::fs::create_dir_all(work_dir)?;
    let bin = work_dir.join("smm_handler.bin");
    write_text_section(elf, &bin)?;

    Ok(SmmHandlerImage {
        code: std::fs::read(&bin)?,
        entry_offset: find_symbol_offset(elf, "fstart_smm_handler")?,
    })
}

pub fn handler_from_rlibs(deps_dir: &Path, work_dir: &Path) -> Result<SmmHandlerImage, BuildError> {
    std::fs::create_dir_all(work_dir)?;
    let elf = work_dir.join("smm_handler.elf");
    let mut inputs = rlibs_in(deps_dir)?;
    inputs.extend(sysroot_rlibs()?);
    if inputs.is_empty() {
        return Err(BuildError::Tool(format!(
            "no rlibs found in {}",
            deps_dir.display()
        )));
    }

    let mut cmd = Command::new("ld");
    cmd.arg("-nostdlib")
        .arg("-Ttext=0")
        .arg("--oformat=elf64-x86-64")
        .arg("--unresolved-symbols=ignore-all")
        // Drop unreachable sections: the SMM rlibs contain every driver in
        // the tree (all platforms, raminit, formatting), but the handler
        // root only needs the southbridge PMIO/TCO path plus install code.
        // Objects are built with -Z function-sections (see
        // build_board_smm_stage) so each function/data item lands in its own
        // section.
        .arg("--gc-sections")
        // Root GC at the SMM handler itself: the default `_start` entry
        // would otherwise pull the entire stage world (console formatting,
        // CAR setup, …) into the SMRAM blob.
        .arg("-e")
        .arg("fstart_smm_handler")
        .arg("-o")
        .arg(&elf)
        .arg("-u")
        .arg("fstart_smm_handler")
        .arg("--start-group");
    for input in &inputs {
        cmd.arg(input);
    }
    cmd.arg("--end-group");
    run_tool(&mut cmd)?;
    audit_smm_blob(&elf)?;
    handler_from_elf(&elf, work_dir)
}

/// Audit the linked SMM blob for SMRAM soundness before extraction.
///
/// The installed blob is a raw `.text`-only copy into SMRAM, linked at
/// `-Ttext=0` with no loader: only relative addressing is correct at any
/// load base. Four independent failures, one loud build error instead of a
/// triple-fault:
/// 1. GOT-indirect calls/jumps through data slots (unresolvable).
/// 2. Allocated data sections with content (unshipped `.rodata`/`.data`/
///    `.got` would read as SMRAM garbage through RIP-relative access).
/// 3. Absolute relocations in shipped sections (baked link addresses).
/// 4. Rust panic machinery (its format strings live in unshipped `.rodata`,
///    and SMM has no console to report to; mirrored from CrabEFI's runtime
///    image audit).
fn audit_smm_blob(elf: &Path) -> Result<(), BuildError> {
    let data = std::fs::read(elf)?;
    let file = object::File::parse(data.as_slice())
        .map_err(|e| BuildError::Tool(format!("failed to parse ELF {}: {e}", elf.display())))?;
    assert_no_got_indirects(elf, &file)?;
    assert_shipped_sections_only(elf, &file)?;
    assert_no_absolute_relocs(elf, &file)?;
    assert_no_panic_symbols(elf, &file)?;
    Ok(())
}

/// Reject SMM handler blobs that need a dynamic loader.
///
/// The installed blob is a raw byte copy into SMRAM: RIP-relative
/// indirect calls/jumps through `.got`/`.data` would resolve to link-time
/// addresses and fault on entry. Any remaining GOT-indirect whose slot
/// lives in a data section fails the build loudly instead of producing a
/// blob that triple-faults.
fn assert_no_got_indirects(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    let text = file
        .section_by_name(".text")
        .ok_or_else(|| BuildError::Tool(format!(".text section not found in {}", elf.display())))?;
    let text_vma = text.address();
    let bytes = text.data().map_err(|e| {
        BuildError::Tool(format!("failed to read .text from {}: {e}", elf.display()))
    })?;
    let mut data_ranges = Vec::new();
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        if matches!(
            name,
            ".got" | ".got.plt" | ".data" | ".data.rel.ro" | ".rodata" | ".fstart.keep"
        ) {
            data_ranges.push((section.address(), section.address() + section.size()));
        }
    }
    if let Some((off, slot)) = find_got_indirect(text_vma, bytes, &data_ranges) {
        return Err(BuildError::Tool(format!(
            "SMM blob calls through GOT at .text+{off:#x} (slot {slot:#x}); \
             force-inline the SMI path into fstart_smm_handler",
        )));
    }
    Ok(())
}

/// Reject allocated data sections with content in the linked blob.
///
/// Only `.text*` ships (`write_text_section`); `.fstart.keep` holds the
/// entry marker consumed by the assembler. Anything else allocated
/// (`.rodata`, `.data*`, `.got*`) would be read as SMRAM garbage through
/// RIP-relative access, so its mere presence fails the build.
fn assert_shipped_sections_only(_elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    use object::elf::{SHF_ALLOC, SHF_EXECINSTR};
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        let (is_alloc, is_exec) = match section.flags() {
            SectionFlags::Elf { sh_flags } => (
                sh_flags & u64::from(SHF_ALLOC) != 0,
                sh_flags & u64::from(SHF_EXECINSTR) != 0,
            ),
            _ => (false, false),
        };
        if forbidden_section(name, is_alloc, is_exec, section.size()) {
            return Err(BuildError::Tool(format!(
                "SMM blob contains allocated {name} ({} bytes); only .text ships to SMRAM",
                section.size(),
            )));
        }
    }
    Ok(())
}

/// Whether an ELF section may not exist with content in the SMM blob.
/// Pure over (name, is_alloc, is_executable, size); see
/// [`assert_shipped_sections_only`].
fn forbidden_section(name: &str, is_alloc: bool, is_executable: bool, size: u64) -> bool {
    if !is_alloc || size == 0 {
        return false;
    }
    // Extraction ships the single merged .text only: a surviving .text.foo
    // would be silently dropped, so it fails loudly instead.
    if is_executable {
        return name != ".text";
    }
    name != ".fstart.keep"
}

/// Reject absolute relocations in shipped sections.
///
/// A static link resolves relative relocations; anything absolute left in
/// shipped bytes (`R_X86_64_64/32`, GOT flavors) is a link-time address
/// that faults in SMRAM. Pure predicate over the kind in
/// [`find_absolute_reloc`]; the [`audit_smm_blob`] wrapper scopes the scan
/// to shipped sections.
fn assert_no_absolute_relocs(_elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        if !(name == ".text" || name.starts_with(".text.")) {
            continue;
        }
        let mut relocs = Vec::new();
        for (offset, reloc) in section.relocations() {
            relocs.push((offset, reloc.kind()));
        }
        if let Some((offset, kind)) = find_absolute_reloc(&relocs) {
            return Err(BuildError::Tool(format!(
                "SMM blob has absolute relocation {kind:?} at {name}+{offset:#x}; \
                 only relative addressing survives the SMRAM copy",
            )));
        }
    }
    Ok(())
}

/// First relocation with a link-absolute kind, if any.
/// Pure over (offset, kind) pairs; see [`assert_no_absolute_relocs`].
fn find_absolute_reloc(relocs: &[(u64, RelocationKind)]) -> Option<(u64, RelocationKind)> {
    relocs.iter().find_map(|&(offset, kind)| {
        matches!(
            kind,
            RelocationKind::Absolute
                | RelocationKind::Got
                | RelocationKind::GotRelative
                | RelocationKind::GotBaseRelative
                | RelocationKind::GotBaseOffset
        )
        .then_some((offset, kind))
    })
}

/// Substrings identifying Rust panic machinery in symbol names, mirrored
/// from CrabEFI's runtime image audit: any of these linked into the blob
/// means a panic path survived GC, and its format strings live in
/// unshipped `.rodata` (and SMM has no console to report to anyway).
const PANIC_SYMBOL_MARKERS: &[&str] = &[
    "rust_begin_unwind",
    "panic_is_possible",
    "panicking",
    "panic_fmt",
    "panic_bounds_check",
    "unwrap_failed",
    "expect_failed",
    "slice_index_fail",
    "len_mismatch_fail",
    "handle_alloc_error",
];

/// Reject Rust panic machinery linked into the SMM blob.
fn assert_no_panic_symbols(_elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    let names: Vec<&str> = file.symbols().filter_map(|s| s.name().ok()).collect();
    if let Some(hit) = find_panic_symbol(names.iter().copied()) {
        return Err(BuildError::Tool(format!(
            "SMM blob links Rust panic symbol {hit}; SMM code must handle errors \
             without panicking (format strings live in unshipped .rodata)",
        )));
    }
    Ok(())
}

/// First panic-machinery symbol name, if any.
/// Pure over symbol names; see [`assert_no_panic_symbols`].
fn find_panic_symbol<'a>(mut names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    names.find(|name| PANIC_SYMBOL_MARKERS.iter().any(|m| name.contains(m)))
}

/// Scan `.text` bytes for RIP-relative indirect calls/jumps whose slot lives
/// in a data section. Returns the first hit as (text offset, slot address).
/// Pure to stay unit-testable; see [`assert_no_got_indirects`].
fn find_got_indirect(
    text_vma: u64,
    bytes: &[u8],
    data_ranges: &[(u64, u64)],
) -> Option<(usize, u64)> {
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        // Optional REX prefix, then FF /2 (call) or FF /4 (jmp) with
        // ModRM mod=00 rm=101 (RIP-relative).
        let (modrm_off, insn_len) =
            if bytes[i] == 0xFF && (bytes[i + 1] == 0x15 || bytes[i + 1] == 0x25) {
                (i + 1, 6u64)
            } else if (0x40..0x50).contains(&bytes[i])
                && i + 7 <= bytes.len()
                && bytes[i + 1] == 0xFF
                && (bytes[i + 2] == 0x15 || bytes[i + 2] == 0x25)
            {
                (i + 2, 7u64)
            } else {
                i += 1;
                continue;
            };
        let disp = i32::from_le_bytes([
            bytes[modrm_off + 1],
            bytes[modrm_off + 2],
            bytes[modrm_off + 3],
            bytes[modrm_off + 4],
        ]) as i64;
        let slot = text_vma
            .wrapping_add(i as u64)
            .wrapping_add(insn_len)
            .wrapping_add(disp as u64);
        if data_ranges
            .iter()
            .any(|&(base, end)| slot >= base && slot < end)
        {
            return Some((i, slot));
        }
        i += 1;
    }
    None
}

fn rlibs_in(dir: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "rlib") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn sysroot_rlibs() -> Result<Vec<PathBuf>, BuildError> {
    let output = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()?;
    if !output.status.success() {
        return Err(BuildError::Tool(format!(
            "rustc --print sysroot failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let sysroot = String::from_utf8_lossy(&output.stdout);
    let lib_dir = Path::new(sysroot.trim()).join("lib/rustlib/x86_64-unknown-none/lib");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&lib_dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|ext| ext == "rlib")
            && (name.starts_with("libcore-")
                || name.starts_with("liballoc-")
                || name.starts_with("libcompiler_builtins-"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn write_text_section(elf: &Path, bin: &Path) -> Result<(), BuildError> {
    let data = std::fs::read(elf)?;
    let file = object::File::parse(data.as_slice())
        .map_err(|e| BuildError::Tool(format!("failed to parse ELF {}: {e}", elf.display())))?;
    let section = file
        .section_by_name(".text")
        .ok_or_else(|| BuildError::Tool(format!(".text section not found in {}", elf.display())))?;
    let text = section.data().map_err(|e| {
        BuildError::Tool(format!("failed to read .text from {}: {e}", elf.display()))
    })?;
    std::fs::write(bin, text)?;
    Ok(())
}

fn find_symbol_offset(elf: &Path, symbol: &str) -> Result<usize, BuildError> {
    let output = Command::new("nm")
        .arg("--defined-only")
        .arg("--numeric-sort")
        .arg(elf)
        .output()?;
    if !output.status.success() {
        return Err(BuildError::Tool(format!(
            "nm failed for {}: {}",
            elf.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == symbol {
            return usize::from_str_radix(parts[0], 16)
                .map_err(|e| BuildError::Tool(format!("bad nm address for {symbol}: {e}")));
        }
    }
    Err(BuildError::Tool(format!(
        "symbol {symbol} not found in {}",
        elf.display()
    )))
}

fn run_tool(cmd: &mut Command) -> Result<(), BuildError> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(BuildError::Tool(format!(
            "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
            cmd,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn validate_options(options: ImageOptions) -> Result<(), BuildError> {
    if options.entry_count == 0 {
        return Err(BuildError::NoEntries);
    }
    if options.entry_count as usize > MAX_SMM_CPUS {
        return Err(BuildError::TooManyEntries);
    }
    if options.stack_size == 0 {
        return Err(BuildError::BadStackSize);
    }
    Ok(())
}

fn put_header(image: &mut [u8], off: usize, h: &SmmImageHeader) {
    put_u32(image, off, h.magic);
    put_u16(image, off + 4, h.version);
    put_u16(image, off + 6, h.header_size);
    put_u32(image, off + 8, h.flags);
    put_u32(image, off + 12, h.image_size);
    put_u16(image, off + 16, h.entry_count);
    put_u16(image, off + 18, h.entry_desc_size);
    put_u32(image, off + 20, h.entries_offset);
    put_u32(image, off + 24, h.common_offset);
    put_u32(image, off + 28, h.common_size);
    put_u32(image, off + 32, h.common_entry_offset);
    put_u32(image, off + 36, h.runtime_offset);
    put_u32(image, off + 40, h.module_args_offset);
    put_u32(image, off + 44, h.module_args_size);
    put_u32(image, off + 48, h.stack_size);
}

fn put_entry_descriptor(image: &mut [u8], off: usize, d: &EntryDescriptor) {
    put_u32(image, off, d.stub_offset);
    put_u32(image, off + 4, d.stub_size);
    put_u32(image, off + 8, d.entry_offset);
    put_u32(image, off + 12, d.params_offset);
}

fn put_u16(image: &mut [u8], off: usize, v: u16) {
    image[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(image: &mut [u8], off: usize, v: u32) {
    image[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn align_up(value: usize, align: usize) -> Result<usize, BuildError> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(BuildError::Overflow)
}

fn as_u32(value: usize) -> Result<u32, BuildError> {
    u32::try_from(value).map_err(|_| BuildError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_smm::header::{HeaderError, SmmImageHeader};

    #[test]
    fn got_indirect_scan_finds_rip_relative_calls() {
        // call *0x100(%rip) at offset 0 with .text at 0: slot = 6 + 0x100.
        let bytes = [0xff, 0x15, 0x00, 0x01, 0x00, 0x00, 0x90];
        assert_eq!(
            find_got_indirect(0, &bytes, &[(0x100, 0x200)]),
            Some((0, 0x106)),
        );
        // Direct relative call is not an indirect.
        let direct = [0xe8, 0x00, 0x01, 0x00, 0x00, 0x90];
        assert_eq!(find_got_indirect(0, &direct, &[(0x100, 0x200)]), None);
        // REX-prefixed jmp through GOT is caught too.
        let jmp = [0x48, 0xff, 0x25, 0xf9, 0x00, 0x00, 0x00];
        assert_eq!(
            find_got_indirect(0, &jmp, &[(0x100, 0x200)]),
            Some((0, 0x100)),
        );
        // Indirect through a slot inside .text (jump table) is fine.
        let table = [0xff, 0x15, 0x00, 0x01, 0x00, 0x00, 0x90];
        assert_eq!(find_got_indirect(0, &table, &[(0x1000, 0x1100)]), None);
    }

    #[test]
    fn shipped_sections_only_permits_text_and_keep_marker() {
        // Exact .text ships; the keep marker is assembler-consumed.
        assert!(!forbidden_section(".text", true, true, 100));
        assert!(!forbidden_section(".fstart.keep", true, false, 8));
        // Unshipped data with content fails, empty sections pass.
        assert!(forbidden_section(".rodata", true, false, 10));
        assert!(forbidden_section(".data.rel.ro", true, false, 8));
        assert!(forbidden_section(".got", true, false, 8));
        assert!(!forbidden_section(".data", true, false, 0));
        // Non-allocated sections (debug info) never ship.
        assert!(!forbidden_section(".debug_info", false, false, 100));
        // Unmerged .text.foo would be silently dropped by extraction.
        assert!(forbidden_section(".text.unlikely", true, true, 16));
    }

    #[test]
    fn absolute_reloc_scan_rejects_got_and_address_kinds() {
        use object::RelocationKind;
        assert_eq!(
            find_absolute_reloc(&[
                (0x10, RelocationKind::Relative),
                (0x20, RelocationKind::PltRelative),
            ]),
            None,
        );
        assert_eq!(
            find_absolute_reloc(&[(0x30, RelocationKind::Absolute)]),
            Some((0x30, RelocationKind::Absolute)),
        );
        assert_eq!(
            find_absolute_reloc(&[(0x40, RelocationKind::GotRelative)]),
            Some((0x40, RelocationKind::GotRelative)),
        );
    }

    #[test]
    fn panic_symbol_scan_matches_machinery_not_handler() {
        assert_eq!(
            find_panic_symbol(["fstart_smm_handler", "FSTART_SMM_KEEP", "main"].into_iter()),
            None,
        );
        assert_eq!(
            find_panic_symbol(["core::panicking::panic_fmt"].into_iter()),
            Some("core::panicking::panic_fmt"),
        );
        assert_eq!(
            find_panic_symbol(["my_unwrap_failed_helper"].into_iter()),
            Some("my_unwrap_failed_helper"),
        );
    }

    fn test_handler() -> SmmHandlerImage {
        SmmHandlerImage {
            code: vec![0xcc],
            entry_offset: 0,
        }
    }

    #[test]
    fn builds_parseable_image_with_four_entries() {
        let built = build_image(
            ImageOptions {
                entry_count: 4,
                stack_size: 0x400,
                coreboot_module_args: true,
                coreboot_header: true,
            },
            &test_handler(),
        )
        .unwrap();

        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_eq!(header.entry_count, 4);
        assert_eq!(header.stack_size, 0x400);
        assert_ne!(header.module_args_offset, 0);
        assert_ne!(header.runtime_offset, 0);

        for i in 0..4 {
            let entry = header.entry(&built.image, i).unwrap();
            assert_ne!(entry.params_offset, 0);
            assert!(
                entry.stub_size as usize
                    >= entry.params_offset as usize + size_of::<SmmEntryParams>()
            );
        }

        let c_header = built.coreboot_header.unwrap();
        assert!(c_header.contains("FSTART_SMM_ENTRY_COUNT 4u"));
        assert!(c_header.contains("FSTART_SMM_MODULE_ARGS_OFFSET"));
    }

    #[test]
    fn rejects_zero_entries() {
        let err = build_image(
            ImageOptions {
                entry_count: 0,
                stack_size: 0x400,
                coreboot_module_args: false,
                coreboot_header: false,
            },
            &test_handler(),
        )
        .unwrap_err();
        assert!(matches!(err, BuildError::NoEntries));
    }

    #[test]
    fn generated_header_matches_blob_offsets() {
        let built = build_image(
            ImageOptions {
                entry_count: 2,
                stack_size: 0x800,
                coreboot_module_args: false,
                coreboot_header: true,
            },
            &test_handler(),
        )
        .unwrap();
        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_eq!(
            header.entry(&built.image, 2).unwrap_err(),
            HeaderError::NotEnoughEntries
        );
        let c_header = built.coreboot_header.unwrap();
        assert!(c_header.contains(&format!(
            "FSTART_SMM_ENTRIES_OFFSET {}u",
            header.entries_offset
        )));
        assert!(c_header.contains(&format!(
            "FSTART_SMM_RUNTIME_OFFSET {}u",
            header.runtime_offset
        )));
    }
}
