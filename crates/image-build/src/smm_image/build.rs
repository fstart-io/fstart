use std::collections::{BTreeMap, BTreeSet};
use std::mem::{align_of, size_of};
use std::path::{Path, PathBuf};
use std::process::Command;

use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, Register};
use object::{
    Object, ObjectSection, ObjectSymbol, RelocationKind, RelocationTarget, SectionFlags,
    SectionKind, SymbolKind,
};

use zerocopy::IntoBytes;

use super::ImageOptions;

use fstart_smm::header::{
    CorebootOffsets, EntryDescriptor, FLAG_COREBOOT_HEADER, FLAG_COREBOOT_MODULE_ARGS,
    FLAG_PROTECTED_MODE, SmmImageHeader, render_coreboot_header,
};
use fstart_smm::layout::SMM_HANDLER_ALIGNMENT;
#[cfg(test)]
use fstart_smm::runtime::SmmEntryParams;
use fstart_smm::runtime::{
    CorebootModuleArgs, HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY, SmmRuntime,
};

#[derive(Debug)]
pub enum BuildError {
    NoEntries,
    BadStackSize,
    BadHandler,
    Overflow,
    Io(std::io::Error),
    Tool(String),
}
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEntries => write!(f, "SMM image must contain at least one entry point"),
            Self::BadStackSize => write!(f, "SMM stack size must be non-zero"),
            Self::BadHandler => write!(f, "SMM handler memory image is invalid"),
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

#[derive(Debug, Clone)]
pub struct BuiltImage {
    pub image: Vec<u8>,
    pub coreboot_header: Option<String>,
}

/// Linked handler bytes plus the complete memory extent including BSS.
#[derive(Debug, Clone)]
pub struct SmmHandlerImage {
    initialized: Vec<u8>,
    memory_size: usize,
    entry_offset: usize,
    /// 32-bit protected-mode handler, linked at 0 and rebased by `fixups`.
    protected_mode: bool,
    /// Handler offsets of 32-bit absolute addresses (protected mode only).
    fixups: Vec<u32>,
}

/// SMM handler execution mode, chosen by the stage's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmmArch {
    /// x86_64 handler: position independent, entered in long mode.
    LongMode,
    /// i686 handler: flat 32-bit protected mode without paging.
    ProtectedMode,
}

impl SmmArch {
    /// Mode for a Rust target triple or target-spec name.
    #[must_use]
    pub fn from_target(target: &str) -> Self {
        if target.starts_with("x86_64") {
            Self::LongMode
        } else {
            Self::ProtectedMode
        }
    }

    const fn ld_emulation(self) -> &'static str {
        match self {
            Self::LongMode => "elf_x86_64",
            Self::ProtectedMode => "elf_i386",
        }
    }
}

struct EntryStub {
    bytes: Vec<u8>,
    params_offset: usize,
}

/// Both stub variants use the same x86-64 ELF container; the define changes
/// the entry code, not the object format.
fn build_entry_stub(out_dir: &Path, protected_mode: bool) -> Result<EntryStub, BuildError> {
    std::fs::create_dir_all(out_dir)?;
    let source = out_dir.join("entry_stub.S");
    let object = out_dir.join("entry_stub.o");
    let elf = out_dir.join("entry_stub.elf");
    write_if_changed(&source, include_bytes!("../../asm/entry_stub.S"))?;

    let build = || {
        let mut assemble = Command::new("cc");
        if protected_mode {
            assemble.arg("-DFSTART_PROTECTED_MODE");
        }
        run_tool(
            assemble
                .args(["-c", "-x", "assembler-with-cpp"])
                .arg(&source)
                .arg("-o")
                .arg(&object),
        )?;
        run_tool(
            Command::new("ld")
                .args(["-nostdlib", "-Ttext=0", "--oformat=elf64-x86-64", "-o"])
                .arg(&elf)
                .arg(&object),
        )?;
        find_symbol_offset(&elf, "fstart_smm_stub_params")
    };
    let params_offset = build().map_err(|error| {
        BuildError::Tool(format!(
            "building the x86 SMM entry stub requires an x86 C compiler and GNU binutils: {error}"
        ))
    })?;
    let data = std::fs::read(&elf)?;
    let file = object::File::parse(data.as_slice())
        .map_err(|e| BuildError::Tool(format!("failed to parse ELF {}: {e}", elf.display())))?;
    let text = file
        .section_by_name(".text")
        .ok_or_else(|| BuildError::Tool(format!("no .text in {}", elf.display())))?;
    let bytes = text
        .data()
        .map_err(|e| BuildError::Tool(format!("could not read .text in {}: {e}", elf.display())))?
        .to_vec();
    Ok(EntryStub {
        bytes,
        params_offset,
    })
}

fn build_image(
    options: ImageOptions,
    handler: &SmmHandlerImage,
    stub: &EntryStub,
) -> Result<BuiltImage, BuildError> {
    validate_options(options)?;
    if handler.initialized.is_empty()
        || handler.memory_size < handler.initialized.len()
        || handler.entry_offset >= handler.initialized.len()
    {
        return Err(BuildError::BadHandler);
    }
    // coreboot's loader neither rebases fixups nor enters 32-bit handlers.
    if handler.protected_mode && (options.coreboot_header || options.coreboot_module_args) {
        return Err(BuildError::Tool(
            "coreboot SMM loading requires a long-mode handler".into(),
        ));
    }
    let header_size = size_of::<SmmImageHeader>();
    let desc_size = size_of::<EntryDescriptor>();
    let entries_offset = header_size;
    let handler_offset = entries_offset
        .checked_add(desc_size * options.entry_count as usize)
        .ok_or(BuildError::Overflow)?
        .checked_next_multiple_of(16)
        .ok_or(BuildError::Overflow)?;
    let stubs_offset = handler_offset
        .checked_add(handler.initialized.len())
        .ok_or(BuildError::Overflow)?
        .checked_next_multiple_of(16)
        .ok_or(BuildError::Overflow)?;
    let stub_size = stub.bytes.len();
    let fixups_offset = stubs_offset
        .checked_add(stub_size * options.entry_count as usize)
        .ok_or(BuildError::Overflow)?;
    let image_size = fixups_offset
        .checked_add(4 * handler.fixups.len())
        .ok_or(BuildError::Overflow)?;

    let runtime_offset = handler
        .memory_size
        .checked_next_multiple_of(align_of::<SmmRuntime>().max(HANDLER_CONFIG_ALIGNMENT))
        .ok_or(BuildError::Overflow)?;
    let runtime_size = size_of::<SmmRuntime>();
    let handler_config_offset = runtime_offset
        .checked_add(runtime_size)
        .ok_or(BuildError::Overflow)?
        .checked_next_multiple_of(HANDLER_CONFIG_ALIGNMENT)
        .ok_or(BuildError::Overflow)?;
    let handler_config_capacity = HANDLER_CONFIG_CAPACITY;
    let mut handler_mem_size = handler_config_offset
        .checked_add(handler_config_capacity)
        .ok_or(BuildError::Overflow)?;
    let module_args_offset = if options.coreboot_module_args {
        let offset = handler_mem_size
            .checked_next_multiple_of(align_of::<CorebootModuleArgs>())
            .ok_or(BuildError::Overflow)?;
        handler_mem_size = offset
            .checked_add(
                size_of::<CorebootModuleArgs>()
                    .checked_mul(options.entry_count as usize)
                    .ok_or(BuildError::Overflow)?,
            )
            .ok_or(BuildError::Overflow)?;
        offset
    } else {
        0
    };

    let mut flags = 0;
    if options.coreboot_module_args {
        flags |= FLAG_COREBOOT_MODULE_ARGS;
    }
    if options.coreboot_header {
        flags |= FLAG_COREBOOT_HEADER;
    }
    if handler.protected_mode {
        flags |= FLAG_PROTECTED_MODE;
    }
    let header = SmmImageHeader::new(
        flags,
        as_u32(image_size)?,
        options.entry_count,
        as_u32(entries_offset)?,
        as_u32(handler_offset)?,
        as_u32(handler.initialized.len())?,
        as_u32(handler_mem_size)?,
        as_u32(handler.entry_offset)?,
        as_u32(runtime_offset)?,
        as_u32(runtime_size)?,
        as_u32(handler_config_offset)?,
        as_u32(handler_config_capacity)?,
        as_u32(module_args_offset)?,
        if options.coreboot_module_args {
            as_u32(size_of::<CorebootModuleArgs>() * options.entry_count as usize)?
        } else {
            0
        },
        options.stack_size,
    )
    .with_fixups(
        if handler.fixups.is_empty() {
            0
        } else {
            as_u32(fixups_offset)?
        },
        as_u32(handler.fixups.len())?,
    );

    let mut image = vec![0u8; image_size];
    header
        .write_to_prefix(&mut image)
        .expect("header space reserved");
    for i in 0..options.entry_count as usize {
        let stub_offset = stubs_offset + i * stub_size;
        EntryDescriptor {
            stub_offset: as_u32(stub_offset)?,
            stub_size: as_u32(stub_size)?,
            entry_offset: 0,
            params_offset: as_u32(stub.params_offset)?,
        }
        .write_to_prefix(&mut image[entries_offset + i * desc_size..])
        .expect("descriptor space reserved");
        image[stub_offset..stub_offset + stub_size].copy_from_slice(&stub.bytes);
    }
    for (i, fixup) in handler.fixups.iter().enumerate() {
        let at = fixups_offset + 4 * i;
        image[at..at + 4].copy_from_slice(&fixup.to_le_bytes());
    }
    image[handler_offset..handler_offset + handler.initialized.len()]
        .copy_from_slice(&handler.initialized);

    let coreboot_header = options.coreboot_header.then(|| {
        render_coreboot_header(
            CorebootOffsets {
                native_header: 0,
                entries: entries_offset as u32,
                handler: handler_offset as u32,
                handler_entry: handler.entry_offset as u32,
                handler_load_size: handler.initialized.len() as u32,
                handler_mem_size: handler_mem_size as u32,
                runtime: runtime_offset as u32,
                module_args: module_args_offset as u32,
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

fn write_image(
    options: ImageOptions,
    handler: &SmmHandlerImage,
    stub: &EntryStub,
    image_path: &Path,
    header_path: Option<&Path>,
) -> Result<(), BuildError> {
    let built = build_image(options, handler, stub)?;
    if let Some(parent) = image_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_if_changed(image_path, &built.image)?;
    if let Some(path) = header_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_changed(
            path,
            built.coreboot_header.as_deref().unwrap_or("").as_bytes(),
        )?;
    }
    Ok(())
}

/// Produce the SMM image (and the coreboot offsets header, if
/// requested) from the rlibs a Cargo build of the handler library
/// reported: select this unit's rlibs out of `artifact_filenames`
/// (those under `target_dir`), link the handler from them and write
/// the image into `out_dir`. Returns the named artefacts produced
/// (`image`, plus `header` when requested). This is the whole
/// SMM-module format production; plan executors only feed it the
/// compiler's reported filenames.
pub fn produce<'a>(
    artifact_filenames: impl Iterator<Item = &'a Path>,
    target_dir: &Path,
    target: &str,
    out_dir: &Path,
    options: ImageOptions,
) -> Result<Vec<(String, PathBuf)>, BuildError> {
    let rlibs = out_dir.join("selected-rlibs");
    if rlibs.exists() {
        std::fs::remove_dir_all(&rlibs)?;
    }
    std::fs::create_dir_all(&rlibs)?;
    for path in artifact_filenames
        .filter(|p| p.starts_with(target_dir) && p.extension().is_some_and(|e| e == "rlib"))
    {
        let name = path
            .file_name()
            .ok_or_else(|| BuildError::Tool("artifact has no filename".into()))?;
        std::fs::copy(path, rlibs.join(name))?;
    }
    let handler = handler_from_rlibs_with_budget(
        &rlibs,
        &out_dir.join("link"),
        SmmArch::from_target(target),
        Some(options.stack_size),
    )?;
    let stub = build_entry_stub(&out_dir.join("stub"), handler.protected_mode)?;
    let image = out_dir.join("image.bin");
    let header = options.coreboot_header.then(|| out_dir.join("offsets.h"));
    write_image(options, &handler, &stub, &image, header.as_deref())?;
    let mut artifacts = vec![("image".to_string(), image)];
    if let Some(header) = header {
        artifacts.push(("header".to_string(), header));
    }
    Ok(artifacts)
}

/// Consumers track the image by mtime (`rerun-if-changed`); rewriting
/// identical bytes would rebuild them on every build.
fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    std::fs::write(path, contents)
}

const LINKER_SCRIPT: &str = r#"
ENTRY(fstart_smm_handler)
SECTIONS {
  . = 0;
  .text : ALIGN(16) { *(.text .text.*) }
  .rodata : ALIGN(16) {
    KEEP(*(.rodata.fstart.heap))
    *(.rodata .rodata.* .srodata .srodata.*)
  }
  .data : ALIGN(16) { *(.data .data.* .sdata .sdata.*) }
  .bss (NOLOAD) : ALIGN(16) {
    KEEP(*(.bss.fstart.heap))
    *(.bss .bss.* .sbss .sbss.*) *(COMMON)
  }
  .stack_sizes 0 : { *(.stack_sizes) }
  /DISCARD/ : { *(.fstart.keep) *(.eh_frame .eh_frame.*) *(.note .note.*) *(.comment) }
}
"#;

fn write_linker_script(work_dir: &Path) -> Result<PathBuf, BuildError> {
    let path = work_dir.join("smm-handler.ld");
    std::fs::write(&path, LINKER_SCRIPT)?;
    Ok(path)
}

pub fn handler_from_archive(
    archive: &Path,
    work_dir: &Path,
) -> Result<SmmHandlerImage, BuildError> {
    std::fs::create_dir_all(work_dir)?;
    let elf = work_dir.join("smm_handler.elf");
    let script = write_linker_script(work_dir)?;
    run_tool(
        Command::new("ld")
            .arg("-nostdlib")
            .arg("--gc-sections")
            .arg("--emit-relocs")
            .arg("--exclude-libs")
            .arg("ALL")
            .arg("-Bsymbolic")
            .arg("-T")
            .arg(&script)
            .arg("-m")
            .arg(SmmArch::LongMode.ld_emulation())
            .arg("-e")
            .arg("fstart_smm_handler")
            .arg("-u")
            .arg("fstart_smm_handler")
            .arg("-o")
            .arg(&elf)
            .arg("--whole-archive")
            .arg(archive)
            .arg("--no-whole-archive"),
    )?;
    handler_from_elf(&elf, work_dir)
}

pub fn handler_from_elf(elf: &Path, work_dir: &Path) -> Result<SmmHandlerImage, BuildError> {
    handler_from_elf_with_budget(elf, work_dir, None)
}

fn handler_from_elf_with_budget(
    elf: &Path,
    _work_dir: &Path,
    stack_budget: Option<u32>,
) -> Result<SmmHandlerImage, BuildError> {
    let fixups = audit_smm_blob_with_budget(elf, stack_budget)?;
    let data = std::fs::read(elf)?;
    let file = object::File::parse(data.as_slice())
        .map_err(|e| BuildError::Tool(format!("failed to parse ELF {}: {e}", elf.display())))?;
    let mut handler = extract_handler(elf, &file)?;
    handler.protected_mode = !file.is_64();
    handler.fixups = fixups;
    Ok(handler)
}

pub fn handler_from_rlibs(
    deps_dir: &Path,
    work_dir: &Path,
    arch: SmmArch,
) -> Result<SmmHandlerImage, BuildError> {
    handler_from_rlibs_with_budget(deps_dir, work_dir, arch, None)
}

fn handler_from_rlibs_with_budget(
    deps_dir: &Path,
    work_dir: &Path,
    arch: SmmArch,
    stack_budget: Option<u32>,
) -> Result<SmmHandlerImage, BuildError> {
    std::fs::create_dir_all(work_dir)?;
    let elf = work_dir.join("smm_handler.elf");
    let script = write_linker_script(work_dir)?;
    let mut inputs = rlibs_in(deps_dir)?;
    // A custom target builds core/alloc with -Zbuild-std; those rlibs are
    // already among the deps.
    if arch == SmmArch::LongMode {
        inputs.extend(sysroot_rlibs()?);
    }
    if inputs.is_empty() {
        return Err(BuildError::Tool(format!(
            "no rlibs found in {}",
            deps_dir.display()
        )));
    }
    let mut cmd = Command::new("ld");
    cmd.arg("-nostdlib")
        .arg("--gc-sections")
        .arg("--emit-relocs")
        .arg("--exclude-libs")
        .arg("ALL")
        .arg("-Bsymbolic")
        .arg("-T")
        .arg(&script)
        .arg("-m")
        .arg(arch.ld_emulation())
        .arg("-e")
        .arg("fstart_smm_handler")
        .arg("-u")
        .arg("fstart_smm_handler")
        .arg("-o")
        .arg(&elf)
        .arg("--start-group");
    for input in &inputs {
        cmd.arg(input);
    }
    cmd.arg("--end-group");
    run_tool(&mut cmd)?;
    handler_from_elf_with_budget(&elf, work_dir, stack_budget)
}

fn extract_handler(elf: &Path, file: &object::File<'_>) -> Result<SmmHandlerImage, BuildError> {
    let mut load_end = 0usize;
    let mut memory_end = 0usize;
    for section in file.sections() {
        if !is_allocated(&section) || section.size() == 0 {
            continue;
        }
        let name = section.name().unwrap_or("");
        if !is_handler_section(name) {
            continue;
        }
        let start = usize::try_from(section.address()).map_err(|_| BuildError::Overflow)?;
        let end = start
            .checked_add(section.size() as usize)
            .ok_or(BuildError::Overflow)?;
        memory_end = memory_end.max(end);
        if section.kind() != SectionKind::UninitializedData {
            load_end = load_end.max(end);
        }
    }
    if load_end == 0 || memory_end < load_end {
        return Err(BuildError::BadHandler);
    }
    let mut initialized = vec![0u8; load_end];
    for section in file.sections() {
        if !is_allocated(&section)
            || section.size() == 0
            || section.kind() == SectionKind::UninitializedData
        {
            continue;
        }
        let name = section.name().unwrap_or("");
        if !is_handler_section(name) {
            continue;
        }
        let start = section.address() as usize;
        let data = section.data().map_err(|e| {
            BuildError::Tool(format!("failed to read {name} from {}: {e}", elf.display()))
        })?;
        initialized[start..start + data.len()].copy_from_slice(data);
    }
    Ok(SmmHandlerImage {
        initialized,
        memory_size: memory_end,
        entry_offset: find_symbol_offset(elf, "fstart_smm_handler")?,
        protected_mode: false,
        fixups: Vec::new(),
    })
}

/// Audit a linked handler and return its absolute-address fixups.
#[cfg(test)]
fn audit_smm_blob(elf: &Path) -> Result<Vec<u32>, BuildError> {
    audit_smm_blob_with_budget(elf, None)
}

fn audit_smm_blob_with_budget(
    elf: &Path,
    stack_budget: Option<u32>,
) -> Result<Vec<u32>, BuildError> {
    let data = std::fs::read(elf)?;
    let file = object::File::parse(data.as_slice())
        .map_err(|e| BuildError::Tool(format!("failed to parse ELF {}: {e}", elf.display())))?;
    assert_alloc_sections(elf, &file)?;
    let fixups = if file.is_64() {
        assert_no_got_indirects(elf, &file)?;
        assert_relative_relocations_only(elf, &file)?;
        Vec::new()
    } else {
        absolute_fixups(elf, &file)?
    };
    assert_no_undefined_symbols(elf, &file)?;
    assert_no_traps_or_panic_calls(elf, &file)?;
    assert_no_panic_symbols(elf, &file)?;
    if let Some(budget) = stack_budget {
        audit_stack_sizes(elf, &file, budget)?;
    }
    Ok(fixups)
}

/// i386 has no PC-relative data addressing, so a protected-mode handler is
/// linked at 0 with absolute addresses. Return the offset of every 32-bit
/// absolute field for the loader to rebase; PC-relative fields move with the
/// image. Every reference must stay inside the copied image.
fn absolute_fixups(elf: &Path, file: &object::File<'_>) -> Result<Vec<u32>, BuildError> {
    let image_end = file
        .sections()
        .filter(|section| is_allocated(section) && is_handler_section(section.name().unwrap_or("")))
        .map(|section| section.address() + section.size())
        .max()
        .unwrap_or(0);
    let in_handler = |index| {
        file.section_by_index(index).is_ok_and(|section| {
            is_allocated(&section) && is_handler_section(section.name().unwrap_or(""))
        })
    };
    let mut fixups = Vec::new();
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        if !is_handler_section(name) {
            continue;
        }
        let data = section.data().unwrap_or(&[]);
        // Linked-executable relocation offsets are virtual addresses.
        for (address, relocation) in section.relocations() {
            let offset = address.wrapping_sub(section.address());
            let target_ok = match relocation.target() {
                RelocationTarget::Symbol(index) => file
                    .symbol_by_index(index)
                    .ok()
                    .and_then(|symbol| symbol.section_index())
                    .is_some_and(in_handler),
                RelocationTarget::Section(index) => in_handler(index),
                _ => false,
            };
            let error = |what: &str| {
                BuildError::Tool(format!(
                    "SMM blob relocation {:?} at {name}+{offset:#x} {what} in {}",
                    relocation.kind(),
                    elf.display()
                ))
            };
            if !target_ok {
                return Err(error("targets an undefined or uncopied address"));
            }
            match (relocation.kind(), relocation.size()) {
                (RelocationKind::Relative | RelocationKind::PltRelative, 32) => {}
                (RelocationKind::Absolute, 32) => {
                    let value = data
                        .get(offset as usize..offset as usize + 4)
                        .map(|b| u32::from_le_bytes(b.try_into().expect("four bytes")))
                        .ok_or_else(|| error("is outside its section"))?;
                    if u64::from(value) > image_end {
                        return Err(error("resolves outside the copied image"));
                    }
                    fixups.push(u32::try_from(address).map_err(|_| BuildError::Overflow)?);
                }
                _ => return Err(error("is not supported")),
            }
        }
    }
    fixups.sort_unstable();
    Ok(fixups)
}

fn is_allocated<'data>(section: &impl ObjectSection<'data>) -> bool {
    matches!(section.flags(), SectionFlags::Elf { sh_flags } if sh_flags & u64::from(object::elf::SHF_ALLOC) != 0)
}
fn is_handler_section(name: &str) -> bool {
    matches!(name, ".text" | ".rodata" | ".data" | ".bss")
}
fn forbidden_section(name: &str, is_alloc: bool, size: u64) -> bool {
    is_alloc && size != 0 && !is_handler_section(name)
}
fn assert_alloc_sections(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        if is_allocated(&section) && section.size() != 0 {
            let alignment = section.align().max(1);
            if !alignment.is_power_of_two()
                || alignment > SMM_HANDLER_ALIGNMENT
                || section.address() % alignment != 0
            {
                return Err(BuildError::Tool(format!(
                    "SMM section {name} requires unsupported alignment {alignment} at {:#x} in {}",
                    section.address(),
                    elf.display()
                )));
            }
        }
        if forbidden_section(name, is_allocated(&section), section.size()) {
            return Err(BuildError::Tool(format!(
                "SMM blob contains unsupported allocated section {name} ({} bytes) in {}",
                section.size(),
                elf.display()
            )));
        }
    }
    Ok(())
}

fn assert_no_got_indirects(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    let text = file
        .section_by_name(".text")
        .ok_or_else(|| BuildError::Tool(format!(".text section not found in {}", elf.display())))?;
    let bytes = text
        .data()
        .map_err(|e| BuildError::Tool(format!("failed to read .text: {e}")))?;
    let data_ranges = [".got", ".got.plt", ".data", ".rodata"]
        .into_iter()
        .filter_map(|name| file.section_by_name(name))
        .map(|section| (section.address(), section.address() + section.size()))
        .collect::<Vec<_>>();
    if let Some((off, slot)) = find_got_indirect(text.address(), bytes, &data_ranges) {
        return Err(BuildError::Tool(format!(
            "SMM blob calls through data/GOT at .text+{off:#x} (slot {slot:#x})"
        )));
    }
    Ok(())
}

fn assert_relative_relocations_only(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    for section in file.sections() {
        let name = section.name().unwrap_or("");
        if !is_handler_section(name) {
            continue;
        }
        for (offset, relocation) in section.relocations() {
            if !matches!(
                relocation.kind(),
                RelocationKind::Relative | RelocationKind::PltRelative
            ) {
                return Err(BuildError::Tool(format!(
                    "SMM blob has load-base-dependent relocation {:?} at {name}+{offset:#x} in {}",
                    relocation.kind(),
                    elf.display()
                )));
            }
            let bias = relative_pc_bias(&section, offset, &relocation);
            let target_ok = match relocation.target() {
                RelocationTarget::Symbol(index) => file
                    .symbol_by_index(index)
                    .ok()
                    .filter(|symbol| !symbol.is_undefined())
                    .and_then(|symbol| symbol.section_index().map(|section| (symbol, section)))
                    .and_then(|(symbol, index)| {
                        file.section_by_index(index)
                            .ok()
                            .map(|section| (symbol, section))
                    })
                    .is_some_and(|(symbol, section)| {
                        is_allocated(&section)
                            && is_handler_section(section.name().unwrap_or(""))
                            && address_in_section(symbol.address(), &section)
                            && bias
                                .and_then(|bias| {
                                    effective_relative_target(symbol.address(), &relocation, bias)
                                })
                                .is_some_and(|address| address_in_copied_image(file, address))
                    }),
                RelocationTarget::Section(index) => file
                    .section_by_index(index)
                    .ok()
                    .filter(|section| {
                        is_allocated(section) && is_handler_section(section.name().unwrap_or(""))
                    })
                    .and_then(|section| {
                        bias.and_then(|bias| {
                            effective_relative_target(section.address(), &relocation, bias)
                        })
                        .map(|address| (address, section))
                    })
                    .is_some_and(|(address, _)| address_in_copied_image(file, address)),
                _ => false,
            };
            if !target_ok {
                return Err(BuildError::Tool(format!(
                    "SMM blob relocation at {name}+{offset:#x} targets an undefined or uncopied address in {}",
                    elf.display()
                )));
            }
        }
    }
    Ok(())
}

fn address_in_section<'data>(address: u64, section: &impl ObjectSection<'data>) -> bool {
    section
        .address()
        .checked_add(section.size())
        .is_some_and(|end| address >= section.address() && address < end)
}

fn address_in_copied_image(file: &object::File<'_>, address: u64) -> bool {
    file.sections().any(|section| {
        is_allocated(&section)
            && is_handler_section(section.name().unwrap_or(""))
            && address_in_section(address, &section)
    })
}

/// ELF uses the relocation field's address as P. x86 instructions instead
/// use next RIP, which may follow an immediate as well as the displacement.
fn relative_pc_bias<'data>(
    section: &impl ObjectSection<'data>,
    offset: u64,
    relocation: &object::Relocation,
) -> Option<i64> {
    let size = relocation.size();
    if size == 0 || !size.is_multiple_of(8) {
        return None;
    }
    // Data PC-relative fields are relative to their own address, not next RIP.
    if section.kind() != SectionKind::Text {
        return Some(0);
    }
    let bytes = section.data().ok()?;
    let mut decoder = Decoder::with_ip(64, bytes, section.address(), DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        let start = instruction.ip().checked_sub(section.address())?;
        if start > offset {
            break;
        }
        let constants = decoder.get_constant_offsets(&instruction);
        let (field_offset, field_size) = if instruction.is_ip_rel_memory_operand() {
            (
                constants.displacement_offset(),
                constants.displacement_size(),
            )
        } else if matches!(
            instruction.op0_kind(),
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
        ) {
            (constants.immediate_offset(), constants.immediate_size())
        } else {
            continue;
        };
        if !instruction.is_invalid()
            && field_size == usize::from(size / 8)
            && start.checked_add(field_offset as u64)? == offset
        {
            let place = section.address().checked_add(offset)?;
            return i64::try_from(instruction.next_ip().checked_sub(place)?).ok();
        }
    }
    None
}

fn effective_relative_target(base: u64, relocation: &object::Relocation, bias: i64) -> Option<u64> {
    add_signed(base, relocation.addend().checked_add(bias)?)
}

fn add_signed(base: u64, addend: i64) -> Option<u64> {
    if addend >= 0 {
        base.checked_add(addend as u64)
    } else {
        base.checked_sub(addend.unsigned_abs())
    }
}

fn assert_no_undefined_symbols(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    if let Some(name) = file.symbols().find_map(|symbol| {
        symbol
            .is_undefined()
            .then(|| symbol.name().ok())
            .flatten()
            .filter(|name| !name.is_empty())
    }) {
        return Err(BuildError::Tool(format!(
            "SMM blob has undefined symbol {name} in {}",
            elf.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
fn find_unsafe_reloc(relocs: &[(u64, RelocationKind)]) -> Option<(u64, RelocationKind)> {
    relocs
        .iter()
        .copied()
        .find(|(_, kind)| !matches!(kind, RelocationKind::Relative | RelocationKind::PltRelative))
}

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
fn assert_no_panic_symbols(_elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    let names = file.symbols().filter_map(|symbol| symbol.name().ok());
    if let Some(hit) = find_panic_symbol(names) {
        return Err(BuildError::Tool(format!(
            "SMM blob links Rust panic symbol {hit}"
        )));
    }
    Ok(())
}
fn find_panic_symbol<'a>(mut names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    names.find(|name| is_panic_symbol(name))
}

fn is_panic_symbol(name: &str) -> bool {
    PANIC_SYMBOL_MARKERS
        .iter()
        .any(|marker| name.contains(marker))
}

#[derive(Debug, Clone)]
struct TextFunction {
    start: u64,
    end: u64,
    name: String,
}

fn collect_text_functions(
    file: &object::File<'_>,
    text_index: object::SectionIndex,
) -> Vec<TextFunction> {
    let mut by_start: BTreeMap<u64, TextFunction> = BTreeMap::new();
    for symbol in file.symbols() {
        if symbol.is_undefined()
            || symbol.section_index() != Some(text_index)
            || symbol.kind() != SymbolKind::Text
            || symbol.size() == 0
        {
            continue;
        }
        let Ok(name) = symbol.name() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let start = symbol.address();
        let Some(end) = start.checked_add(symbol.size()) else {
            continue;
        };
        by_start
            .entry(start)
            .and_modify(|existing| {
                // Prefer the entry symbol name or longer range if aliases share an address.
                let longer = end > existing.end;
                existing.end = existing.end.max(end);
                if name == "fstart_smm_handler" || (existing.name != "fstart_smm_handler" && longer)
                {
                    existing.name = name.to_string();
                }
            })
            .or_insert_with(|| TextFunction {
                start,
                end,
                name: name.to_string(),
            });
    }
    by_start.into_values().collect()
}

fn enclosing_symbol_name<'a>(
    functions: &'a [TextFunction],
    text_symbols: &'a [(u64, String)],
    addr: u64,
) -> String {
    if let Some(func) = functions.iter().find(|f| addr >= f.start && addr < f.end) {
        return format!("{}+{:#x}", func.name, addr - func.start);
    }
    if let Some((sym_addr, name)) = text_symbols.iter().rev().find(|(a, _)| *a <= addr) {
        return format!("{name}+{:#x}", addr - sym_addr);
    }
    format!("{addr:#x}")
}

/// Disassemble `.text` with `iced-x86` and reject:
/// - hardware trap opcodes (`ud2`, `ud1`, `ud0`) anywhere in `.text` (LLVM
///   emits these for `llvm.trap`, unreachable/UB traps, and panic-abort paths
///   even when no panic symbol is linked);
/// - `int3` inside any `STT_FUNC` body (inter-function `0xcc` padding from the
///   linker is outside function spans and ignored);
/// - direct `call` or branch instructions targeting a panic symbol, with
///   caller function attribution.
fn assert_no_traps_or_panic_calls(elf: &Path, file: &object::File<'_>) -> Result<(), BuildError> {
    let Some(text) = file.section_by_name(".text") else {
        return Ok(());
    };
    let bytes = text
        .data()
        .map_err(|e| BuildError::Tool(format!("failed to read .text in {}: {e}", elf.display())))?;
    if bytes.is_empty() {
        return Ok(());
    }
    let text_vma = text.address();
    let text_end = text_vma
        .checked_add(bytes.len() as u64)
        .ok_or(BuildError::Overflow)?;
    let bitness = if file.is_64() { 64 } else { 32 };
    let functions = collect_text_functions(file, text.index());

    let mut text_symbols: Vec<(u64, String)> = file
        .symbols()
        .filter(|s| !s.is_undefined() && s.section_index() == Some(text.index()))
        .filter_map(|s| {
            let name = s.name().ok()?;
            (!name.is_empty()).then(|| (s.address(), name.to_string()))
        })
        .collect();
    text_symbols.sort_by_key(|(addr, _)| *addr);

    let mut panic_targets: BTreeMap<u64, String> = BTreeMap::new();
    for symbol in file.symbols() {
        if let Ok(name) = symbol.name()
            && !symbol.is_undefined()
            && is_panic_symbol(name)
        {
            panic_targets.insert(symbol.address(), name.to_string());
        }
    }

    let check_slice = |slice_vma: u64, slice: &[u8], inside_func: bool| -> Result<(), BuildError> {
        let mut decoder = Decoder::with_ip(bitness, slice, slice_vma, DecoderOptions::NONE);
        while decoder.can_decode() {
            let insn = decoder.decode();
            let ip = insn.ip();
            match insn.mnemonic() {
                Mnemonic::Ud0
                | Mnemonic::Ud1
                | Mnemonic::Ud2
                | Mnemonic::Int
                | Mnemonic::Int1
                | Mnemonic::Into => {
                    let loc = enclosing_symbol_name(&functions, &text_symbols, ip);
                    return Err(BuildError::Tool(format!(
                        "SMM blob contains trap instruction {:?} at {loc} in {}",
                        insn.mnemonic(),
                        elf.display()
                    )));
                }
                Mnemonic::Int3 if inside_func => {
                    let loc = enclosing_symbol_name(&functions, &text_symbols, ip);
                    return Err(BuildError::Tool(format!(
                        "SMM blob contains Int3 trap instruction at {loc} in {}",
                        elf.display()
                    )));
                }
                _ => {}
            }
            if matches!(
                insn.op0_kind(),
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
            ) {
                let target = insn.near_branch_target();
                if let Some(panic_name) = panic_targets.get(&target) {
                    let loc = enclosing_symbol_name(&functions, &text_symbols, ip);
                    return Err(BuildError::Tool(format!(
                        "SMM blob branches to panic symbol {panic_name} from {loc} in {}",
                        elf.display()
                    )));
                }
            }
        }
        Ok(())
    };

    // Decode each STT_FUNC body cleanly (strict: Int3 rejected), and decode
    // any gaps between/outside STT_FUNC spans (allowing linker 0xcc padding
    // while still rejecting Ud0/Ud1/Ud2 and panic branches).
    let mut cursor = text_vma;
    for func in &functions {
        if func.start < text_vma || func.end > text_end || func.start >= func.end {
            continue;
        }
        if func.start > cursor {
            let gap_start = (cursor - text_vma) as usize;
            let gap_end = (func.start - text_vma) as usize;
            check_slice(cursor, &bytes[gap_start..gap_end], false)?;
        }
        let f_start = (func.start - text_vma) as usize;
        let f_end = (func.end - text_vma) as usize;
        check_slice(func.start, &bytes[f_start..f_end], true)?;
        cursor = cursor.max(func.end);
    }
    if cursor < text_end {
        let gap_start = (cursor - text_vma) as usize;
        check_slice(cursor, &bytes[gap_start..], false)?;
    }
    Ok(())
}

fn read_uleb128(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        let value = u64::from(byte & 0x7f);
        if shift >= 64 || (shift == 63 && value > 1) {
            return None;
        }
        result |= value << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
}

/// Parse ELF `.stack_sizes` entries: `(function_vma, frame_bytes)`.
fn parse_stack_sizes(
    elf: &Path,
    file: &object::File<'_>,
) -> Result<Option<BTreeMap<u64, u64>>, BuildError> {
    let Some(section) = file.section_by_name(".stack_sizes") else {
        return Ok(None);
    };
    let data = section.data().map_err(|e| {
        BuildError::Tool(format!(
            "failed to read .stack_sizes in {}: {e}",
            elf.display()
        ))
    })?;
    let addr_len = if file.is_64() { 8 } else { 4 };
    let mut cursor = 0usize;
    let mut map = BTreeMap::new();
    while cursor < data.len() {
        let addr_bytes = data.get(cursor..cursor + addr_len).ok_or_else(|| {
            BuildError::Tool(format!(
                "truncated .stack_sizes address at offset {cursor:#x} in {}",
                elf.display()
            ))
        })?;
        let addr = if file.is_64() {
            u64::from_le_bytes(addr_bytes.try_into().expect("8 bytes"))
        } else {
            u64::from(u32::from_le_bytes(addr_bytes.try_into().expect("4 bytes")))
        };
        cursor += addr_len;
        let size = read_uleb128(data, &mut cursor).ok_or_else(|| {
            BuildError::Tool(format!(
                "invalid ULEB128 in .stack_sizes at offset {cursor:#x} in {}",
                elf.display()
            ))
        })?;
        map.entry(addr)
            .and_modify(|existing: &mut u64| *existing = (*existing).max(size))
            .or_insert(size);
    }
    Ok(Some(map))
}

/// Prove via `iced-x86` disassembly that a `.stack_sizes`-less function (such
/// as `compiler_builtins`'s naked assembly `memcpy`/`memset`/`memmove`) is a
/// pure leaf that never modifies the stack pointer and never transfers control
/// outside `[func_start, func_end)` except via plain `ret`.
fn is_zero_stack_leaf(bitness: u32, func_bytes: &[u8], func_start: u64, func_end: u64) -> bool {
    if func_bytes.is_empty() {
        return false;
    }
    let mut decoder = Decoder::with_ip(bitness, func_bytes, func_start, DecoderOptions::NONE);
    while decoder.can_decode() {
        let insn = decoder.decode();
        if insn.is_invalid() {
            return false;
        }
        match insn.mnemonic() {
            Mnemonic::Call
            | Mnemonic::Push
            | Mnemonic::Pusha
            | Mnemonic::Pushad
            | Mnemonic::Pushf
            | Mnemonic::Pushfd
            | Mnemonic::Pushfq
            | Mnemonic::Pop
            | Mnemonic::Popa
            | Mnemonic::Popad
            | Mnemonic::Popf
            | Mnemonic::Popfd
            | Mnemonic::Popfq
            | Mnemonic::Enter
            | Mnemonic::Leave
            | Mnemonic::Retf
            | Mnemonic::Iret
            | Mnemonic::Iretd
            | Mnemonic::Iretq
            | Mnemonic::Int
            | Mnemonic::Int1
            | Mnemonic::Int3
            | Mnemonic::Into
            | Mnemonic::Syscall
            | Mnemonic::Sysenter
            | Mnemonic::Sysexit
            | Mnemonic::Sysret
            | Mnemonic::Lss
            | Mnemonic::Ud0
            | Mnemonic::Ud1
            | Mnemonic::Ud2 => return false,
            Mnemonic::Ret if insn.op_count() != 0 => return false,
            Mnemonic::Jmp
                if !matches!(
                    insn.op0_kind(),
                    OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
                ) =>
            {
                return false;
            }
            _ => {}
        }
        for op in 0..insn.op_count() {
            match insn.op_kind(op) {
                OpKind::Register => {
                    if matches!(
                        insn.op_register(op),
                        Register::RSP | Register::ESP | Register::SP | Register::SPL
                    ) {
                        return false;
                    }
                }
                OpKind::Memory => {
                    if matches!(
                        insn.memory_base(),
                        Register::RSP | Register::ESP | Register::SP | Register::SPL
                    ) || matches!(
                        insn.memory_index(),
                        Register::RSP | Register::ESP | Register::SP | Register::SPL
                    ) {
                        return false;
                    }
                }
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    let target = insn.near_branch_target();
                    if target < func_start || target >= func_end {
                        return false;
                    }
                }
                OpKind::FarBranch16 | OpKind::FarBranch32 => return false,
                _ => {}
            }
        }
    }
    true
}

/// If `insn` is `and rsp/esp/sp, -N` with a power-of-two alignment mask `-N`,
/// returns the worst-case stack slack (`N - 1` bytes) introduced by dynamic
/// alignment. Returns `None` if the operand is not a constant power-of-two
/// alignment mask.
fn stack_realign_slack(insn: &iced_x86::Instruction, is_64: bool) -> Option<u64> {
    let imm = match insn.op1_kind() {
        OpKind::Immediate8 => insn.immediate8() as i8 as i64 as u64,
        OpKind::Immediate8to16 => insn.immediate8to16() as i64 as u64,
        OpKind::Immediate8to32 => insn.immediate8to32() as i64 as u64,
        OpKind::Immediate8to64 => insn.immediate8to64() as u64,
        OpKind::Immediate16 => insn.immediate16() as i16 as i64 as u64,
        OpKind::Immediate32 => insn.immediate32() as i32 as i64 as u64,
        OpKind::Immediate32to64 => insn.immediate32to64() as u64,
        OpKind::Immediate64 => insn.immediate64(),
        _ => return None,
    };
    let mask = if is_64 {
        imm
    } else {
        imm | 0xffff_ffff_0000_0000
    };
    let align = mask.wrapping_neg();
    if align >= 2 && align.is_power_of_two() && align <= 0x10000 {
        Some(align - 1)
    } else {
        None
    }
}

/// Static worst-case stack-depth proof from `fstart_smm_handler` using LLVM's
/// `.stack_sizes` metadata and `iced-x86` call-graph reconstruction.
///
/// Note: `.stack_sizes` records compiler-generated stack frames; hand-written
/// inline `asm!` that adjusts `rsp`/`esp` inside a Rust function is not
/// tracked by LLVM's `.stack_sizes` emission. Constant power-of-two stack
/// realignment (`and rsp/esp, -N`) adds `N - 1` bytes of conservative slack;
/// non-constant stack realignment and all indirect calls/jumps (switch jump
/// tables are disabled via `-Cjump-tables=no`) are rejected directly.
fn audit_stack_sizes(elf: &Path, file: &object::File<'_>, budget: u32) -> Result<u64, BuildError> {
    let mut stack_sizes = parse_stack_sizes(elf, file)?.ok_or_else(|| {
        BuildError::Tool(format!(
            "SMM blob is missing .stack_sizes section (compile with -Zemit-stack-sizes) in {}",
            elf.display()
        ))
    })?;
    let text = file
        .section_by_name(".text")
        .ok_or_else(|| BuildError::Tool(format!("no .text section in {}", elf.display())))?;
    let bytes = text
        .data()
        .map_err(|e| BuildError::Tool(format!("failed to read .text in {}: {e}", elf.display())))?;
    let text_vma = text.address();
    let text_end = text_vma
        .checked_add(bytes.len() as u64)
        .ok_or(BuildError::Overflow)?;
    let functions = collect_text_functions(file, text.index());
    let func_by_addr: BTreeMap<u64, &TextFunction> =
        functions.iter().map(|f| (f.start, f)).collect();

    let entry = functions
        .iter()
        .find(|f| f.name == "fstart_smm_handler")
        .ok_or_else(|| {
            BuildError::Tool(format!(
                "fstart_smm_handler STT_FUNC symbol not found for stack audit in {}",
                elf.display()
            ))
        })?;

    let is_64 = file.is_64();
    let bitness = if is_64 { 64 } else { 32 };
    let ret_addr_bytes = if is_64 { 8u64 } else { 4u64 };
    // Conservative entry-stub overhead: up to 15 bytes from `and $-16, sp`
    // alignment + entry stub call frame (8B return addr on x86_64; 12B align +
    // 4B arg push + 4B return addr on i686) + 8B optional stack-bottom canary.
    let entry_overhead = if is_64 {
        16 + 8 + 8
    } else {
        16 + 12 + 4 + 4 + 8
    };

    // Build outgoing direct-call and tail-call edges for every reachable function.
    // Edge tuples: (callee_addr, call_push_bytes).
    let mut edges: BTreeMap<u64, Vec<(u64, u64)>> = BTreeMap::new();
    let mut queue = vec![entry.start];
    let mut visited = BTreeSet::new();

    while let Some(addr) = queue.pop() {
        if !visited.insert(addr) {
            continue;
        }
        let func = func_by_addr.get(&addr).ok_or_else(|| {
            BuildError::Tool(format!(
                "SMM call target {addr:#x} has no STT_FUNC symbol in {}",
                elf.display()
            ))
        })?;
        if func.start < text_vma || func.end > text_end {
            return Err(BuildError::Tool(format!(
                "SMM function {} lies outside .text in {}",
                func.name,
                elf.display()
            )));
        }
        let start_off = (func.start - text_vma) as usize;
        let end_off = (func.end - text_vma) as usize;
        let func_bytes = &bytes[start_off..end_off];
        if let std::collections::btree_map::Entry::Vacant(entry) = stack_sizes.entry(addr) {
            if is_zero_stack_leaf(bitness, func_bytes, func.start, func.end) {
                entry.insert(0);
            } else {
                return Err(BuildError::Tool(format!(
                    "SMM reachable function {} ({addr:#x}) has no .stack_sizes entry in {}",
                    func.name,
                    elf.display()
                )));
            }
        }
        let mut decoder = Decoder::with_ip(bitness, func_bytes, func.start, DecoderOptions::NONE);
        let mut out = Vec::new();
        let mut align_slack = 0u64;
        while decoder.can_decode() {
            let insn = decoder.decode();
            if insn.mnemonic() == Mnemonic::And
                && insn.op0_kind() == OpKind::Register
                && matches!(
                    insn.op0_register(),
                    Register::RSP | Register::ESP | Register::SP
                )
            {
                if let Some(slack) = stack_realign_slack(&insn, is_64) {
                    align_slack = align_slack.checked_add(slack).ok_or(BuildError::Overflow)?;
                } else {
                    return Err(BuildError::Tool(format!(
                        "SMM function {}+{:#x} dynamically realigns the stack pointer, breaking static stack-depth proof in {}",
                        func.name,
                        insn.ip() - func.start,
                        elf.display()
                    )));
                }
            }
            if insn.mnemonic() == Mnemonic::Call {
                if matches!(
                    insn.op0_kind(),
                    OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
                ) {
                    let target = insn.near_branch_target();
                    out.push((target, ret_addr_bytes));
                    queue.push(target);
                } else {
                    return Err(BuildError::Tool(format!(
                        "SMM function {}+{:#x} performs an indirect call, breaking static stack-depth proof in {}",
                        func.name,
                        insn.ip() - func.start,
                        elf.display()
                    )));
                }
            } else if insn.mnemonic() == Mnemonic::Jmp
                && !matches!(
                    insn.op0_kind(),
                    OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
                )
            {
                return Err(BuildError::Tool(format!(
                    "SMM function {}+{:#x} performs an indirect jump, breaking static stack-depth proof in {}",
                    func.name,
                    insn.ip() - func.start,
                    elf.display()
                )));
            } else if matches!(
                insn.op0_kind(),
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
            ) {
                let target = insn.near_branch_target();
                if target < func.start || target >= func.end {
                    // Tail call or cross-function branch: does not push a new
                    // return address, but the callee executes on this stack.
                    out.push((target, 0));
                    queue.push(target);
                }
            }
        }
        if align_slack != 0 {
            let entry = stack_sizes.get_mut(&addr).unwrap();
            *entry = entry.checked_add(align_slack).ok_or(BuildError::Overflow)?;
        }
        edges.insert(addr, out);
    }

    // DFS with cycle detection and memoization.
    let mut memo: BTreeMap<u64, (u64, Vec<(String, u64)>)> = BTreeMap::new();
    let mut active: Vec<u64> = Vec::new();

    #[allow(clippy::type_complexity)]
    fn dfs(
        addr: u64,
        elf: &Path,
        func_by_addr: &BTreeMap<u64, &TextFunction>,
        stack_sizes: &BTreeMap<u64, u64>,
        edges: &BTreeMap<u64, Vec<(u64, u64)>>,
        active: &mut Vec<u64>,
        memo: &mut BTreeMap<u64, (u64, Vec<(String, u64)>)>,
    ) -> Result<(u64, Vec<(String, u64)>), BuildError> {
        if let Some(cached) = memo.get(&addr) {
            return Ok(cached.clone());
        }
        if let Some(pos) = active.iter().position(|&a| a == addr) {
            let cycle = active[pos..]
                .iter()
                .chain(core::iter::once(&addr))
                .map(|a| func_by_addr[a].name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(BuildError::Tool(format!(
                "SMM call graph contains recursion ({cycle}), breaking static stack-depth proof in {}",
                elf.display()
            )));
        }
        active.push(addr);
        let frame = stack_sizes[&addr];
        let mut worst_child = 0u64;
        let mut worst_chain = Vec::new();
        if let Some(callees) = edges.get(&addr) {
            for &(callee, push_bytes) in callees {
                let (callee_total, callee_chain) =
                    dfs(callee, elf, func_by_addr, stack_sizes, edges, active, memo)?;
                let cost = push_bytes
                    .checked_add(callee_total)
                    .ok_or(BuildError::Overflow)?;
                if cost > worst_child {
                    worst_child = cost;
                    worst_chain = callee_chain;
                }
            }
        }
        active.pop();
        let total = frame.checked_add(worst_child).ok_or(BuildError::Overflow)?;
        let mut chain = vec![(func_by_addr[&addr].name.clone(), frame)];
        chain.extend(worst_chain);
        memo.insert(addr, (total, chain.clone()));
        Ok((total, chain))
    }

    let (handler_worst, chain) = dfs(
        entry.start,
        elf,
        &func_by_addr,
        &stack_sizes,
        &edges,
        &mut active,
        &mut memo,
    )?;
    let total = handler_worst
        .checked_add(entry_overhead)
        .ok_or(BuildError::Overflow)?;
    if total > u64::from(budget) {
        let trace = chain
            .iter()
            .map(|(name, frame)| format!("{name} ({frame}B)"))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(BuildError::Tool(format!(
            "SMM worst-case stack {total}B (entry overhead {entry_overhead}B + {trace}) exceeds stack_size budget {budget}B in {}",
            elf.display()
        )));
    }
    Ok(total)
}

fn find_got_indirect(
    text_vma: u64,
    bytes: &[u8],
    data_ranges: &[(u64, u64)],
) -> Option<(usize, u64)> {
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        let (modrm_off, insn_len) = if bytes[i] == 0xff && matches!(bytes[i + 1], 0x15 | 0x25) {
            (i + 1, 6u64)
        } else if (0x40..0x50).contains(&bytes[i])
            && i + 7 <= bytes.len()
            && bytes[i + 1] == 0xff
            && matches!(bytes[i + 2], 0x15 | 0x25)
        {
            (i + 2, 7u64)
        } else {
            i += 1;
            continue;
        };
        let disp = i32::from_le_bytes(bytes[modrm_off + 1..modrm_off + 5].try_into().ok()?) as i64;
        let slot = text_vma
            .wrapping_add(i as u64)
            .wrapping_add(insn_len)
            .wrapping_add(disp as u64);
        if data_ranges
            .iter()
            .any(|&(start, end)| slot >= start && slot < end)
        {
            return Some((i, slot));
        }
        i += 1;
    }
    None
}

fn rlibs_in(dir: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut paths = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rlib"))
        .collect::<Vec<_>>();
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
    let root = String::from_utf8_lossy(&output.stdout);
    let dir = Path::new(root.trim()).join("lib/rustlib/x86_64-unknown-none/lib");
    let mut paths = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            path.extension().is_some_and(|ext| ext == "rlib")
                && (name.starts_with("libcore-")
                    || name.starts_with("liballoc-")
                    || name.starts_with("libcompiler_builtins-"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}
fn find_symbol_offset(elf: &Path, symbol: &str) -> Result<usize, BuildError> {
    let output = Command::new("nm")
        .arg("--defined-only")
        .arg("--numeric-sort")
        .arg(elf)
        .output()?;
    if !output.status.success() {
        return Err(BuildError::Tool(format!("nm failed for {}", elf.display())));
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() >= 3 && parts[2] == symbol {
            return usize::from_str_radix(parts[0], 16)
                .map_err(|e| BuildError::Tool(format!("bad nm address: {e}")));
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
            "command failed: {cmd:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
fn validate_options(options: ImageOptions) -> Result<(), BuildError> {
    if options.entry_count == 0 {
        return Err(BuildError::NoEntries);
    }
    if options.stack_size == 0 {
        return Err(BuildError::BadStackSize);
    }
    Ok(())
}
fn as_u32(value: usize) -> Result<u32, BuildError> {
    u32::try_from(value).map_err(|_| BuildError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_smm::header::{HeaderError, SMM_IMAGE_MAGIC};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

    fn relocation_fixture(
        linker_prefix: &str,
        allow_undefined: bool,
        target_prelude: &str,
        target_addend: &str,
        target_definition: &str,
        data_section_attributes: &str,
    ) -> PathBuf {
        assemble_fixture(
            linker_prefix,
            allow_undefined,
            &format!(
                ".text\n{target_prelude}\n.globl fstart_smm_handler\nfstart_smm_handler:\n call target{target_addend}\n ret\n{target_definition}"
            ),
            data_section_attributes,
        )
    }

    fn assemble_fixture(
        linker_prefix: &str,
        allow_undefined: bool,
        assembly: &str,
        data_section_attributes: &str,
    ) -> PathBuf {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fstart-smm-reloc-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fixture.S");
        let object = dir.join("fixture.o");
        let script = dir.join("fixture.ld");
        let elf = dir.join("fixture.elf");
        std::fs::write(&source, assembly).unwrap();
        std::fs::write(
            &script,
            format!(
                "{linker_prefix}\nSECTIONS {{ . = 0; .text : {{ *(.text*) }} .rodata : {{ *(.rodata*) }} .data {data_section_attributes} : {{ *(.data*) }} .bss : {{ *(.bss*) }} /DISCARD/ : {{ *(*) }} }}\n"
            ),
        )
        .unwrap();
        assert!(
            Command::new("cc")
                .args([
                    "-c",
                    source.to_str().unwrap(),
                    "-o",
                    object.to_str().unwrap()
                ])
                .status()
                .unwrap()
                .success()
        );
        let mut link = Command::new("ld");
        // The fixture image is deliberately one flat RWX segment.
        link.args(["-nostdlib", "--emit-relocs", "--no-warn-rwx-segments"]);
        if allow_undefined {
            link.arg("--unresolved-symbols=ignore-all");
        }
        assert!(
            link.args(["-T", script.to_str().unwrap(), "-o", elf.to_str().unwrap()])
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        elf
    }

    fn test_handler() -> SmmHandlerImage {
        SmmHandlerImage {
            initialized: vec![0xcc, 0x11, 0x22],
            memory_size: 0x40,
            entry_offset: 0,
            protected_mode: false,
            fixups: Vec::new(),
        }
    }

    fn test_stub() -> EntryStub {
        EntryStub {
            bytes: vec![0; size_of::<SmmEntryParams>()],
            params_offset: 0,
        }
    }

    #[test]
    fn builds_unversioned_image_with_bss_runtime_and_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let stub = build_entry_stub(dir.path(), false).unwrap();
        let built = build_image(
            ImageOptions {
                entry_count: 4,
                stack_size: 0x400,
                coreboot_module_args: true,
                coreboot_header: true,
            },
            &test_handler(),
            &stub,
        )
        .unwrap();
        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_eq!(header.runtime_offset as usize % HANDLER_CONFIG_ALIGNMENT, 0);
        assert_eq!(header.magic, SMM_IMAGE_MAGIC);
        assert_eq!(header.handler_load_size, 3);
        assert!(header.handler_mem_size > 0x40);
        assert_eq!(header.runtime_size as usize, size_of::<SmmRuntime>());
        assert_ne!(header.module_args_offset, 0);
        for i in 0..4 {
            let entry = header.entry(&built.image, i).unwrap();
            assert!(
                entry.stub_size as usize
                    >= entry.params_offset as usize + size_of::<SmmEntryParams>()
            );
        }
        let generated = built.coreboot_header.unwrap();
        assert!(generated.contains("FSTART_SMM_HANDLER_LOAD_SIZE 3u"));
        assert!(generated.contains("FSTART_SMM_HANDLER_ALIGNMENT 4096u"));
        assert!(generated.contains("FSTART_SMM_ENTRY_COUNT 4u"));
    }

    #[test]
    fn aligns_runtime_relative_config_when_handler_memory_ends_on_eight_bytes() {
        let mut handler = test_handler();
        handler.memory_size = 0x48;
        let built = build_image(
            ImageOptions {
                entry_count: 1,
                stack_size: 0x400,
                coreboot_module_args: false,
                coreboot_header: false,
            },
            &handler,
            &test_stub(),
        )
        .unwrap();
        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_eq!(header.runtime_offset as usize % HANDLER_CONFIG_ALIGNMENT, 0);
        assert_eq!(
            (header.handler_config_offset - header.runtime_offset) as usize
                % HANDLER_CONFIG_ALIGNMENT,
            0
        );
    }

    #[test]
    fn rejects_zero_entries_and_bad_handler() {
        assert!(matches!(
            build_image(
                ImageOptions {
                    entry_count: 0,
                    stack_size: 1,
                    coreboot_module_args: false,
                    coreboot_header: false
                },
                &test_handler(),
                &test_stub(),
            ),
            Err(BuildError::NoEntries)
        ));
        let bad = SmmHandlerImage {
            initialized: vec![1],
            memory_size: 0,
            entry_offset: 0,
            protected_mode: false,
            fixups: Vec::new(),
        };
        assert!(matches!(
            build_image(
                ImageOptions {
                    entry_count: 1,
                    stack_size: 1,
                    coreboot_module_args: false,
                    coreboot_header: false
                },
                &bad,
                &test_stub(),
            ),
            Err(BuildError::BadHandler)
        ));
    }

    #[test]
    fn relocation_filter_accepts_only_pc_relative() {
        assert_eq!(
            find_unsafe_reloc(&[
                (0, RelocationKind::Relative),
                (4, RelocationKind::PltRelative)
            ]),
            None
        );
        assert_eq!(
            find_unsafe_reloc(&[(8, RelocationKind::Absolute)]),
            Some((8, RelocationKind::Absolute))
        );
        assert_eq!(
            find_unsafe_reloc(&[(8, RelocationKind::GotRelative)]),
            Some((8, RelocationKind::GotRelative))
        );
    }

    #[test]
    fn relocation_audit_rejects_undefined_target() {
        let elf = relocation_fixture("", true, "", "", "", "");
        let error = audit_smm_blob(&elf).unwrap_err().to_string();
        assert!(error.contains("undefined or uncopied"), "{error}");
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
    }

    #[test]
    fn relocation_audit_rejects_defined_target_outside_copied_image() {
        let elf = relocation_fixture("target = 0x100000;", false, "", "", "", "");
        let error = audit_smm_blob(&elf).unwrap_err().to_string();
        assert!(error.contains("undefined or uncopied"), "{error}");
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
    }

    #[test]
    fn relocation_audit_accounts_for_x86_pc_relative_bias() {
        let elf = relocation_fixture("", false, ".globl target\ntarget:\n ret", "", "", "");
        let data = std::fs::read(&elf).unwrap();
        let file = object::File::parse(data.as_slice()).unwrap();
        let text = file.section_by_name(".text").unwrap();
        let (_, relocation) = text.relocations().next().unwrap();
        let RelocationTarget::Symbol(index) = relocation.target() else {
            panic!("expected symbol-target relocation");
        };
        let symbol = file.symbol_by_index(index).unwrap();
        assert_eq!(relocation.addend(), -4);
        assert_eq!(relocation.size(), 32);
        assert_eq!(
            effective_relative_target(symbol.address(), &relocation, 4),
            Some(symbol.address())
        );
        audit_smm_blob(&elf).unwrap();
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();

        let elf = relocation_fixture(
            "",
            false,
            ".section .text.target,\"ax\"\n.globl section_target_marker\nsection_target_marker:\ntarget:\n ret\n.text",
            "",
            "",
            "",
        );
        let data = std::fs::read(&elf).unwrap();
        let file = object::File::parse(data.as_slice()).unwrap();
        let text = file.section_by_name(".text").unwrap();
        let (_, relocation) = text.relocations().next().unwrap();
        let RelocationTarget::Symbol(index) = relocation.target() else {
            panic!("expected ELF section-symbol relocation");
        };
        let section_symbol = file.symbol_by_index(index).unwrap();
        assert_eq!(section_symbol.kind(), object::SymbolKind::Section);
        let marker = file
            .symbols()
            .find(|symbol| symbol.name().ok() == Some("section_target_marker"))
            .unwrap();
        assert_eq!(
            effective_relative_target(section_symbol.address(), &relocation, 4),
            Some(marker.address())
        );
        audit_smm_blob(&elf).unwrap();
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
    }

    #[test]
    fn relocation_audit_handles_immediates_after_rip_displacements() {
        for (store, bias) in [("movb $1,target(%rip)", 5), ("movl $1,target(%rip)", 8)] {
            let elf = assemble_fixture(
                "",
                false,
                &format!(
                    ".text\n.globl fstart_smm_handler\nfstart_smm_handler:\n {store}\n ret\n.bss\n.globl target\ntarget:\n .zero 4\n"
                ),
                "",
            );
            let data = std::fs::read(&elf).unwrap();
            let file = object::File::parse(data.as_slice()).unwrap();
            let text = file.section_by_name(".text").unwrap();
            let (offset, relocation) = text.relocations().next().unwrap();
            assert_eq!(relocation.addend(), -bias);
            assert_eq!(relative_pc_bias(&text, offset, &relocation), Some(bias));
            handler_from_elf(&elf, elf.parent().unwrap()).unwrap();
            std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn handler_alignment_is_bounded_by_the_loader_contract() {
        for alignment in [64, 4096, 8192] {
            let elf = assemble_fixture(
                "",
                false,
                &format!(
                    ".text\n.globl fstart_smm_handler\nfstart_smm_handler:\n ret\n.bss\n.balign {alignment}\n .zero 64\n"
                ),
                "",
            );
            let result = handler_from_elf(&elf, elf.parent().unwrap());
            if alignment <= SMM_HANDLER_ALIGNMENT {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("unsupported alignment")
                );
            }
            std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn relocation_audit_rejects_symbol_addends_outside_copied_image() {
        for addend in ["+0x100000", "-0x100000"] {
            let elf =
                relocation_fixture("", false, "", addend, ".globl target\ntarget:\n ret\n", "");
            let error = audit_smm_blob(&elf).unwrap_err().to_string();
            assert!(error.contains("undefined or uncopied"), "{error}");
            std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn relocation_audit_rejects_target_in_nonallocated_handler_section() {
        let elf = relocation_fixture(
            "",
            false,
            "",
            "",
            ".section .data,\"\",@progbits\n.globl target\ntarget:\n .byte 0\n",
            "(INFO)",
        );
        let error = audit_smm_blob(&elf).unwrap_err().to_string();
        assert!(error.contains("undefined or uncopied"), "{error}");
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
    }

    #[test]
    fn protected_mode_handler_records_absolute_fixups() {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fstart-smm-i386-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (source, object, elf) = (dir.join("f.S"), dir.join("f.o"), dir.join("f.elf"));
        std::fs::write(
            &source,
            ".text\n.globl fstart_smm_handler\nfstart_smm_handler:\n movl $table, %eax\n call target\ntarget:\n ret\n.data\n.balign 4\ntable:\n .long target\n",
        )
        .unwrap();
        let script = write_linker_script(&dir).unwrap();
        for cmd in [
            Command::new("as")
                .args(["--32", "-o"])
                .arg(&object)
                .arg(&source),
            Command::new("ld")
                .args(["-m", "elf_i386", "-nostdlib", "--emit-relocs", "-T"])
                .arg(&script)
                .arg("-o")
                .arg(&elf)
                .arg(&object),
        ] {
            assert!(cmd.status().unwrap().success());
        }

        let handler = handler_from_elf(&elf, &dir).unwrap();
        assert!(handler.protected_mode);
        // movl's imm32 at .text+1 and the pointer in .data.
        assert_eq!(handler.fixups, [1, 0x10]);
        let stub = build_entry_stub(&dir.join("stub"), handler.protected_mode).unwrap();
        let built = build_image(
            ImageOptions {
                entry_count: 1,
                stack_size: 0x400,
                coreboot_module_args: false,
                coreboot_header: false,
            },
            &handler,
            &stub,
        )
        .unwrap();
        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_ne!(header.flags & FLAG_PROTECTED_MODE, 0);
        assert_eq!(header.fixup(&built.image, 1), Ok(0x10));
        let entry = header.entry(&built.image, 0).unwrap();
        assert_eq!(entry.stub_size as usize, stub.bytes.len());
        assert_eq!(entry.params_offset as usize, stub.params_offset);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn section_filter_allows_complete_memory_image_only() {
        for name in [".text", ".rodata", ".data", ".bss"] {
            assert!(!forbidden_section(name, true, 8));
        }
        for name in [".got", ".plt", ".dynamic", ".tdata"] {
            assert!(forbidden_section(name, true, 8));
        }
        assert!(!forbidden_section(".debug_info", false, 8));
    }

    #[test]
    fn got_indirect_scanner_still_rejects_data_calls() {
        let bytes = [0xff, 0x15, 0, 1, 0, 0, 0x90];
        assert_eq!(
            find_got_indirect(0, &bytes, &[(0x100, 0x200)]),
            Some((0, 0x106))
        );
    }

    #[test]
    fn panic_symbol_scan_is_precise() {
        assert_eq!(find_panic_symbol(["fstart_smm_handler"].into_iter()), None);
        assert_eq!(
            find_panic_symbol(["core::panicking::panic_fmt"].into_iter()),
            Some("core::panicking::panic_fmt")
        );
    }

    #[test]
    fn descriptor_bounds_are_checked() {
        let built = build_image(
            ImageOptions {
                entry_count: 1,
                stack_size: 0x400,
                coreboot_module_args: false,
                coreboot_header: false,
            },
            &test_handler(),
            &test_stub(),
        )
        .unwrap();
        let header = SmmImageHeader::parse(&built.image).unwrap();
        assert_eq!(
            header.entry(&built.image, 1),
            Err(HeaderError::NotEnoughEntries)
        );
    }

    fn assemble_with_smm_script(assembly: &str) -> PathBuf {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fstart-smm-audit-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fixture.S");
        let object = dir.join("fixture.o");
        let elf = dir.join("fixture.elf");
        std::fs::write(&source, assembly).unwrap();
        let script = write_linker_script(&dir).unwrap();
        assert!(
            Command::new("cc")
                .args([
                    "-c",
                    source.to_str().unwrap(),
                    "-o",
                    object.to_str().unwrap()
                ])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("ld")
                .args([
                    "-nostdlib",
                    "--emit-relocs",
                    "--no-warn-rwx-segments",
                    "-T",
                    script.to_str().unwrap(),
                    "-o",
                    elf.to_str().unwrap(),
                ])
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        elf
    }

    #[test]
    fn trap_audit_rejects_ud2_and_int3_in_func_but_allows_inter_function_padding() {
        // Inter-function 0xcc padding outside STT_FUNC spans must pass.
        let clean = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call leaf\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .byte 0xcc, 0xcc, 0xcc, 0xcc\n\
             .type leaf, @function\n\
             leaf:\n\
              ret\n\
             .size leaf, .-leaf\n",
        );
        audit_smm_blob(&clean).unwrap();
        std::fs::remove_dir_all(clean.parent().unwrap()).unwrap();

        // ud2 inside a function is rejected with function attribution.
        let with_ud2 = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              ud2\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n",
        );
        let err = audit_smm_blob(&with_ud2).unwrap_err().to_string();
        assert!(err.contains("trap instruction Ud2"), "{err}");
        assert!(err.contains("fstart_smm_handler"), "{err}");
        std::fs::remove_dir_all(with_ud2.parent().unwrap()).unwrap();

        // int3 inside an STT_FUNC body is rejected.
        let with_int3 = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              int3\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n",
        );
        let err = audit_smm_blob(&with_int3).unwrap_err().to_string();
        assert!(err.contains("Int3 trap"), "{err}");
        std::fs::remove_dir_all(with_int3.parent().unwrap()).unwrap();
    }

    #[test]
    fn trap_audit_attributes_direct_call_to_panic_symbol() {
        let elf = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call panic_fmt_stub\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type panic_fmt_stub, @function\n\
             panic_fmt_stub:\n\
              ret\n\
             .size panic_fmt_stub, .-panic_fmt_stub\n",
        );
        let err = audit_smm_blob(&elf).unwrap_err().to_string();
        assert!(
            err.contains("branches to panic symbol panic_fmt_stub"),
            "{err}"
        );
        assert!(err.contains("from fstart_smm_handler"), "{err}");
        std::fs::remove_dir_all(elf.parent().unwrap()).unwrap();
    }

    #[test]
    fn stack_audit_enforces_budget_and_rejects_recursion_and_indirect_calls() {
        // Bounded call graph: entry (40B) -> helper (64B) -> leaf (24B).
        // Worst case = 32B entry overhead + 40 + 8 + 64 + 8 + 24 = 176B.
        let bounded = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call helper\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type helper, @function\n\
             helper:\n\
              call leaf\n\
              ret\n\
             .size helper, .-helper\n\
             .type leaf, @function\n\
             leaf:\n\
              ret\n\
             .size leaf, .-leaf\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 40\n\
             .quad helper\n .uleb128 64\n\
             .quad leaf\n .uleb128 24\n",
        );
        audit_smm_blob_with_budget(&bounded, Some(176)).unwrap();
        let err = audit_smm_blob_with_budget(&bounded, Some(175))
            .unwrap_err()
            .to_string();
        assert!(err.contains("worst-case stack 176B"), "{err}");
        assert!(err.contains("exceeds stack_size budget 175B"), "{err}");
        std::fs::remove_dir_all(bounded.parent().unwrap()).unwrap();

        // Direct recursion must be rejected.
        let recursive = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call recurse\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type recurse, @function\n\
             recurse:\n\
              call recurse\n\
              ret\n\
             .size recurse, .-recurse\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n\
             .quad recurse\n .uleb128 32\n",
        );
        let err = audit_smm_blob_with_budget(&recursive, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("contains recursion"), "{err}");
        std::fs::remove_dir_all(recursive.parent().unwrap()).unwrap();

        // Indirect call must be rejected.
        let indirect = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call *%rax\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        let err = audit_smm_blob_with_budget(&indirect, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("indirect call"), "{err}");
        std::fs::remove_dir_all(indirect.parent().unwrap()).unwrap();

        // Indirect tail-call jumps must also be rejected.
        let indirect_jmp = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              jmp *%rax\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        let err = audit_smm_blob_with_budget(&indirect_jmp, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("indirect jump"), "{err}");
        std::fs::remove_dir_all(indirect_jmp.parent().unwrap()).unwrap();

        // Constant power-of-two stack realignment (`and rsp, -64`) adds N-1 (63B) slack.
        let realign_const = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              and $-64, %rsp\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        // 32B entry overhead + 16B frame + 63B alignment slack = 111B.
        audit_smm_blob_with_budget(&realign_const, Some(111)).unwrap();
        let err = audit_smm_blob_with_budget(&realign_const, Some(110))
            .unwrap_err()
            .to_string();
        assert!(err.contains("worst-case stack 111B"), "{err}");
        std::fs::remove_dir_all(realign_const.parent().unwrap()).unwrap();

        // Non-constant stack realignment (`and %rax, %rsp`) must be rejected.
        let realign_dyn = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              and %rax, %rsp\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        let err = audit_smm_blob_with_budget(&realign_dyn, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("dynamically realigns the stack pointer"),
            "{err}"
        );
        std::fs::remove_dir_all(realign_dyn.parent().unwrap()).unwrap();
    }

    #[test]
    fn stack_audit_accepts_zero_stack_asm_leaf_and_rejects_untracked_stack_use() {
        // Pure 0-stack naked assembly leaf (like compiler_builtins memcpy) has
        // no .stack_sizes entry, never touches RSP, and makes no calls.
        let asm_leaf = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call memcpy_leaf\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type memcpy_leaf, @function\n\
             memcpy_leaf:\n\
              mov %rdi, %rax\n\
              mov %rdx, %rcx\n\
              rep movsb\n\
              ret\n\
             .size memcpy_leaf, .-memcpy_leaf\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        // 32B entry overhead + 16B handler + 8B return address + 0B leaf = 56B.
        audit_smm_blob_with_budget(&asm_leaf, Some(56)).unwrap();
        let err = audit_smm_blob_with_budget(&asm_leaf, Some(55))
            .unwrap_err()
            .to_string();
        assert!(err.contains("worst-case stack 56B"), "{err}");
        std::fs::remove_dir_all(asm_leaf.parent().unwrap()).unwrap();

        // Any .stack_sizes-less function that touches the stack must be rejected.
        let asm_push = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call pushes_rbp\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type pushes_rbp, @function\n\
             pushes_rbp:\n\
              push %rbp\n\
              pop %rbp\n\
              ret\n\
             .size pushes_rbp, .-pushes_rbp\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        let err = audit_smm_blob_with_budget(&asm_push, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no .stack_sizes entry"), "{err}");
        std::fs::remove_dir_all(asm_push.parent().unwrap()).unwrap();

        // RSP-relative memory access (e.g. red-zone write) must also be rejected.
        let asm_redzone = assemble_with_smm_script(
            ".text\n\
             .globl fstart_smm_handler\n\
             .type fstart_smm_handler, @function\n\
             fstart_smm_handler:\n\
              call redzone_leaf\n\
              ret\n\
             .size fstart_smm_handler, .-fstart_smm_handler\n\
             .type redzone_leaf, @function\n\
             redzone_leaf:\n\
              mov %rax, -8(%rsp)\n\
              ret\n\
             .size redzone_leaf, .-redzone_leaf\n\
             .section .stack_sizes,\"o\",@progbits,.text\n\
             .quad fstart_smm_handler\n .uleb128 16\n",
        );
        let err = audit_smm_blob_with_budget(&asm_redzone, Some(0x400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no .stack_sizes entry"), "{err}");
        std::fs::remove_dir_all(asm_redzone.parent().unwrap()).unwrap();
    }
}
