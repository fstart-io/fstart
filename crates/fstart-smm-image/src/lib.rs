//! Host-side builder for standalone fstart PIC SMM images.
//!
//! The produced blob is consumed by fstart platform SMM installers and by the
//! optional coreboot loader integration.  Entry stubs are part of the image:
//! loaders only copy bytes into SMRAM and patch data parameter blocks.

use std::path::{Path, PathBuf};
use std::process::Command;

use fstart_smm::header::{
    render_coreboot_header, CorebootOffsets, EntryDescriptor, SmmImageHeader, FLAG_COREBOOT_HEADER,
    FLAG_COREBOOT_MODULE_ARGS,
};
#[cfg(test)]
use fstart_smm::runtime::SmmEntryParams;
use fstart_smm::runtime::{CorebootModuleArgs, SmmRuntime, MAX_SMM_CPUS};
use fstart_types::SmmPlatform;

#[cfg(not(rust_analyzer))]
mod asm {
    include!(concat!(env!("OUT_DIR"), "/smm_image_asm.rs"));
}

#[cfg(rust_analyzer)]
mod asm {
    pub const ENTRY_STUB: &[u8] = &[];
    pub const ENTRY_PARAMS_OFFSET: usize = 0;
}

/// Errors returned while building or writing an SMM image.
#[derive(Debug)]
pub enum BuildError {
    /// The requested entry count is zero.
    NoEntries,
    /// The requested entry count exceeds the fixed ABI cap.
    TooManyEntries,
    /// The requested stack size is zero.
    BadStackSize,
    /// Integer arithmetic overflowed while laying out the image.
    Overflow,
    /// Filesystem I/O failed.
    Io(std::io::Error),
    /// A tool used to build the generated SMM stage failed.
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

/// Build options for a standalone SMM image.
#[derive(Debug, Clone, Copy)]
pub struct ImageOptions {
    /// Number of precompiled PIC entry stubs to include.
    pub entry_count: u16,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Include a coreboot-compatible module-args block in the handler/data region.
    pub coreboot_module_args: bool,
    /// Mark the image as having been built with a generated coreboot header.
    pub coreboot_header: bool,
    /// SMM handler composition to build into the stage.
    pub platform: SmmPlatform,
}

/// A generated SMM image and its optional coreboot offset header text.
#[derive(Debug, Clone)]
pub struct BuiltImage {
    /// Native image bytes.
    pub image: Vec<u8>,
    /// C header text for the coreboot loader, when requested.
    pub coreboot_header: Option<String>,
}

/// Build a standalone native SMM image.
///
/// Each entry stub is self-contained PIC code.  It starts at the architectural
/// SMM entry point in 16-bit mode, builds a flat GDT from its current SMBASE,
/// enters protected mode, enables long mode using the patched CR3, sets the
/// per-CPU stack from [`fstart_smm::runtime::SmmEntryParams`], calls the copied
/// Rust SMM handler, and finally exits SMM with `rsm`.
pub fn build_image(options: ImageOptions) -> Result<BuiltImage, BuildError> {
    validate_options(options)?;

    let stub = asm::ENTRY_STUB;
    let handler = build_smm_stage(options.platform)?;
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

/// Build and write an SMM image, plus an optional generated coreboot header.
pub fn write_image(
    options: ImageOptions,
    image_path: &Path,
    header_path: Option<&Path>,
) -> Result<BuiltImage, BuildError> {
    let built = build_image(options)?;
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

struct BuiltHandler {
    code: Vec<u8>,
    entry_offset: usize,
}

fn build_smm_stage(platform: SmmPlatform) -> Result<BuiltHandler, BuildError> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or(BuildError::Overflow)?
        .to_path_buf();
    let profile = "release";
    let out_dir = workspace_root
        .join("target")
        .join("smm-stage")
        .join(platform_env(platform));
    let target_dir = out_dir.join("target");
    let elf = out_dir.join("smm_handler.elf");
    let bin = out_dir.join("smm_handler.bin");
    std::fs::create_dir_all(&out_dir)?;

    run_tool(
        Command::new("cargo")
            .arg("rustc")
            .arg("-p")
            .arg("fstart-smm-stage")
            .arg("--target")
            .arg("x86_64-unknown-none")
            .arg("--target-dir")
            .arg(&target_dir)
            .arg("--release")
            .arg("--")
            .arg("-C")
            .arg("panic=abort")
            .arg("-C")
            .arg("opt-level=s")
            .arg("-C")
            .arg("relocation-model=pic")
            .arg("-C")
            .arg("no-redzone=yes")
            .env("FSTART_SMM_PLATFORM", platform_env(platform))
            .current_dir(&workspace_root),
    )?;

    let archive = target_dir
        .join("x86_64-unknown-none")
        .join(profile)
        .join("libfstart_smm_stage.a");
    run_tool(
        Command::new("ld")
            .arg("-nostdlib")
            .arg("-Ttext=0")
            .arg("--oformat=elf64-x86-64")
            .arg("-o")
            .arg(&elf)
            .arg("--whole-archive")
            .arg(&archive)
            .arg("--no-whole-archive"),
    )?;
    run_tool(
        Command::new("objcopy")
            .arg("-O")
            .arg("binary")
            .arg("-j")
            .arg(".text")
            .arg(&elf)
            .arg(&bin),
    )?;

    Ok(BuiltHandler {
        code: std::fs::read(&bin)?,
        entry_offset: find_symbol_offset(&elf, "fstart_smm_handler")?,
    })
}

fn platform_env(platform: SmmPlatform) -> &'static str {
    match platform {
        SmmPlatform::QemuQ35 => "qemu-q35",
        SmmPlatform::PineviewIch7 => "pineview-ich7",
        SmmPlatform::LenovoX61 => "lenovo-x61",
    }
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
    fn builds_parseable_image_with_four_entries() {
        let built = build_image(ImageOptions {
            entry_count: 4,
            stack_size: 0x400,
            coreboot_module_args: true,
            coreboot_header: true,
            platform: SmmPlatform::PineviewIch7,
        })
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
        let err = build_image(ImageOptions {
            entry_count: 0,
            stack_size: 0x400,
            coreboot_module_args: false,
            coreboot_header: false,
            platform: SmmPlatform::PineviewIch7,
        })
        .unwrap_err();
        assert!(matches!(err, BuildError::NoEntries));
    }

    #[test]
    fn generated_header_matches_blob_offsets() {
        let built = build_image(ImageOptions {
            entry_count: 2,
            stack_size: 0x800,
            coreboot_module_args: false,
            coreboot_header: true,
            platform: SmmPlatform::PineviewIch7,
        })
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
