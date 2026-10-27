// SPDX-License-Identifier: MIT OR Apache-2.0
use clap::Parser;
use fstart_image_build::amd::{self, Blob, FlashRegion, Key, PicassoImage};
use std::path::PathBuf;

/// AMD firmware inspection/validation and Picasso partition assembly.
/// The original packing options remain accepted without a subcommand.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(clap::Subcommand)]
enum Command {
    /// Assemble a Picasso partition fragment (not a whole-chip image).
    Pack(Box<PackArgs>),
    /// Inspect the selected firmware region and all selected directory levels.
    Inspect(ImageArgs),
    /// Validate reachable dependencies; optionally check a retained prefix.
    Validate(ImageArgs),
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum Platform {
    Picasso,
    Zeppelin,
    Naples,
    Rome,
    Milan,
}
impl Platform {
    fn profile(self) -> fstart_amdfw_format::Profile {
        use fstart_amdfw_format::{Profile, efs::BiosDirectorySlot as Slot};
        Profile::for_slot(match self {
            Self::Picasso => Slot::Family17hModel10,
            Self::Zeppelin | Self::Naples => Slot::Family17hModel00,
            Self::Rome => Slot::Family17hModel30,
            Self::Milan => Slot::Family19h,
        })
    }
}
#[derive(clap::Args)]
struct ImageArgs {
    image: PathBuf,
    #[arg(long, value_enum)]
    profile: Platform,
    /// Start of the selected image in a multi-image ROM.
    #[arg(long, default_value = "0", value_parser = hex32)]
    image_origin: u32,
    /// Defaults to the top-mapped 16-MiB window (or the smaller flash size).
    #[arg(long, value_parser = hex64)]
    physical_base: Option<u64>,
    #[arg(long, value_parser = hex32)]
    slot_origin: Option<u32>,
    /// Override combo selection for a platform not yet named by this CLI.
    #[arg(long, value_parser = hex32)]
    psp_id: Option<u32>,
    #[arg(long, value_parser = hex32)]
    chip_id: Option<u32>,
    #[arg(long, value_parser = hex32)]
    generation_mask: Option<u32>,
    #[arg(long, value_parser = hex32)]
    prefix_size: Option<u32>,
    /// A replacement reset slot, permitted outside the retained prefix.
    #[arg(long, requires = "reset_size", value_parser = hex32)]
    reset_offset: Option<u32>,
    #[arg(long, requires = "reset_offset", value_parser = hex32)]
    reset_size: Option<u32>,
}

/// Output is a partition fragment, not a full-chip image. Signed AMD
/// firmware is never modified or re-signed.
#[derive(Parser)]
struct PackArgs {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    source_root: PathBuf,
    #[arg(long)]
    blob_directory: Option<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    reset_image: PathBuf,
    #[arg(long, value_parser = hex64)]
    reset_destination: u64,
    #[arg(long, default_value = "0x1000000", value_parser = hex32)]
    flash_size: u32,
    #[arg(long, default_value = "0xe20000", value_parser = hex32)]
    efs_offset: u32,
    #[arg(long, value_parser = hex32)]
    partition_end: u32,
    #[arg(long, default_value = "4")]
    spi_read_mode: u8,
    #[arg(long, default_value = "0")]
    spi_speed: u8,
    #[arg(long)]
    micron: bool,
    // Bit 28 selected by Picasso, bit 29 disables MP2 when not packaged.
    #[arg(long, value_parser = hex64)]
    soft_fuse: Option<u64>,
    #[arg(long)]
    secure_os: bool,
    #[arg(long)]
    mp2: bool,
    #[arg(long)]
    s0i3: bool,
    #[arg(long)]
    apcb: Vec<PathBuf>,
    #[arg(long)]
    microcode: Vec<PathBuf>,
    #[arg(long)]
    verstage: Option<PathBuf>,
    #[arg(long)]
    verstage_signature: Option<PathBuf>,
    #[arg(long, default_value = "0x2001000", value_parser = hex64)]
    apob_destination: u64,
    #[arg(long, requires = "shared_memory_size", value_parser = hex64)]
    shared_memory: Option<u64>,
    #[arg(long, requires = "shared_memory", value_parser = hex32)]
    shared_memory_size: Option<u32>,
    #[arg(long, requires = "apob_nv_size", value_parser = hex32)]
    apob_nv_offset: Option<u32>,
    #[arg(long, requires = "apob_nv_offset", value_parser = hex32)]
    apob_nv_size: Option<u32>,
    #[arg(long, requires = "psp_nv_size", value_parser = hex32)]
    psp_nv_offset: Option<u32>,
    #[arg(long, requires = "psp_nv_offset", value_parser = hex32)]
    psp_nv_size: Option<u32>,
}
fn hex64(value: &str) -> Result<u64, String> {
    let (digits, radix) = value.strip_prefix("0x").map_or((value, 10), |s| (s, 16));
    u64::from_str_radix(digits, radix).map_err(|e| e.to_string())
}
fn hex32(value: &str) -> Result<u32, String> {
    u32::try_from(hex64(value)?).map_err(|_| "number exceeds u32".into())
}
fn read(path: &PathBuf) -> amd::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn pack(a: PackArgs) -> amd::Result<()> {
    let text = std::fs::read_to_string(&a.config).map_err(|e| e.to_string())?;
    let files = amd::config::parse(
        &text,
        &a.source_root,
        a.blob_directory.as_deref(),
        amd::config::Options {
            secure_os: a.secure_os,
            mp2: a.mp2,
            s0i3: a.s0i3,
            secure_debug: false,
        },
    )?;
    let (mut psp, mut bios) = files.read()?;
    for (instance, path) in a.apcb.iter().enumerate() {
        bios.push(Blob {
            key: Key::new(
                0x60,
                0,
                u8::try_from(instance).map_err(|_| "too many APCBs")?,
            ),
            bytes: read(path)?,
        });
    }
    for (instance, path) in a.microcode.iter().enumerate() {
        bios.push(Blob {
            key: Key::new(
                0x66,
                0,
                u8::try_from(instance).map_err(|_| "too many microcodes")?,
            ),
            bytes: read(path)?,
        });
    }
    for (kind, path) in [
        (0x52, a.verstage.as_ref()),
        (0x53, a.verstage_signature.as_ref()),
    ] {
        if let Some(path) = path {
            psp.push(Blob {
                key: Key::new(kind, 0, 0),
                bytes: read(path)?,
            });
        }
    }
    let region = |offset: Option<u32>, size: Option<u32>| {
        offset
            .zip(size)
            .map(|(offset, size)| FlashRegion { offset, size })
    };
    let packed = PicassoImage {
        flash_size: a.flash_size,
        efs_offset: a.efs_offset,
        partition_end: a.partition_end,
        spi_read_mode: a.spi_read_mode,
        spi_speed: a.spi_speed,
        micron: a.micron,
        soft_fuse: a
            .soft_fuse
            .unwrap_or((1 << 28) | if a.mp2 { 0 } else { 1 << 29 }),
        psp,
        bios,
        reset_image: read(&a.reset_image)?,
        reset_destination: a.reset_destination,
        apob_destination: a.apob_destination,
        shared_memory: a.shared_memory.zip(a.shared_memory_size),
        apob_nv: region(a.apob_nv_offset, a.apob_nv_size),
        psp_nv: region(a.psp_nv_offset, a.psp_nv_size),
    }
    .pack()?;
    // create_new prevents accidental overwrite of a backup or factory image.
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&a.output)
        .map_err(|e| format!("{}: {e}", a.output.display()))?;
    output.write_all(&packed.bytes).map_err(|e| e.to_string())?;
    println!(
        "partition fragment: offset=0x{:x} length=0x{:x} PSP=0x{:x} BIOS=0x{:x}",
        packed.offset,
        packed.bytes.len(),
        packed.psp_directory,
        packed.bios_directory
    );
    Ok(())
}
fn inspect(a: ImageArgs, validate: bool) -> amd::Result<()> {
    use fstart_amdfw_format::{
        Flash,
        directory::{Directory, Entry},
    };
    let bytes = read(&a.image)?;
    let origin = a.image_origin as usize;
    let available = bytes
        .len()
        .checked_sub(origin)
        .filter(|&n| n > 0)
        .ok_or("image origin out of range")?;
    let mut profile = a.profile.profile();
    if let Some(id) = a.psp_id {
        profile.psp_id = Some(id);
    }
    if let Some(id) = a.chip_id {
        profile.chip_id = Some(id);
    }
    if let Some(mask) = a.generation_mask {
        profile.generation_mask = mask;
    }
    let base = a
        .physical_base
        .unwrap_or((1u64 << 32) - available.min(16 << 20) as u64);
    let mut flash = Flash::region(&bytes, origin, base, profile);
    if let Some(slot) = a.slot_origin {
        flash = flash.with_slot(slot as usize);
    }
    let error = |e| format!("invalid/unsupported firmware: {e:?}");
    let (efs, _) = flash.efs_checked().map_err(error)?;
    println!("EFS={efs:#x} origin={origin:#x} physical-base={base:#x}");
    fn table<E: Entry>(label: &str, dir: Directory<'_, E>) -> amd::Result<()> {
        println!(
            "{label}={:#x} header=v{} mode={} entries={}",
            dir.offset,
            dir.header.version(),
            dir.header.address_mode(),
            dir.entries.len()
        );
        for entry in dir.entries {
            if entry.has_data() {
                let range = dir
                    .range(entry)
                    .map_err(|e| format!("{label} entry {:#x}: {e:?}", entry.kind()))?;
                println!(
                    "  type={:#04x} sub={} instance={} source={:#x}..{:#x}",
                    entry.kind(),
                    entry.subprogram(),
                    entry.instance(),
                    range.start,
                    range.end
                );
            } else {
                println!("  type={:#04x} value={:#x}", entry.kind(), entry.source());
            }
        }
        Ok(())
    }
    for dir in flash.psp_directories().map_err(error)?.iter() {
        table("PSP", dir)?;
    }
    for dir in flash
        .bios_directories_checked(profile.bios)
        .map_err(error)?
        .iter()
    {
        table("BIOS", dir)?;
    }
    if validate {
        let reset = a
            .reset_offset
            .zip(a.reset_size)
            .map(|(o, s)| {
                let end = o.checked_add(s).ok_or("reset slot overflow")?;
                Ok::<_, &str>(o as usize..end as usize)
            })
            .transpose()?;
        flash
            .validate_prefix(
                profile.bios,
                a.prefix_size.map_or(bytes.len(), |n| n as usize),
                reset,
            )
            .map_err(error)?;
        println!("reachable firmware dependencies validated");
    }
    Ok(())
}
fn arguments(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Command, clap::Error> {
    let args: Vec<_> = args.into_iter().collect();
    // Preserve the original flag-only packing invocation without making
    // required pack fields part of the inspection/validation parser.
    if args
        .get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|a| a.starts_with('-') && !matches!(a, "--help" | "-h" | "--version"))
    {
        PackArgs::try_parse_from(args).map(|a| Command::Pack(Box::new(a)))
    } else {
        Args::try_parse_from(args).map(|a| a.command)
    }
}
fn run(a: Command) -> amd::Result<()> {
    match a {
        Command::Inspect(a) => inspect(a, false),
        Command::Validate(a) => inspect(a, true),
        Command::Pack(a) => pack(*a),
    }
}
fn main() {
    let command = arguments(std::env::args_os()).unwrap_or_else(|e| e.exit());
    if let Err(error) = run(command) {
        eprintln!("amdfw: {error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_does_not_require_pack_inputs() {
        let command =
            arguments(["amdfw", "inspect", "vendor.rom", "--profile", "milan"].map(Into::into))
                .unwrap();
        assert!(matches!(command, Command::Inspect(_)));
    }
    #[test]
    fn legacy_pack_invocation_is_preserved() {
        let command = arguments(
            [
                "amdfw",
                "--config",
                "config",
                "--source-root",
                ".",
                "--output",
                "out",
                "--reset-image",
                "stage",
                "--reset-destination",
                "0x2000000",
                "--partition-end",
                "0x1000000",
            ]
            .map(Into::into),
        )
        .unwrap();
        assert!(matches!(command, Command::Pack(_)));
    }
}
