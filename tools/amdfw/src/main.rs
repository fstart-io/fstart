// SPDX-License-Identifier: MIT OR Apache-2.0
use clap::Parser;
use fstart_image_build::amd::{self, Blob, FlashRegion, Key, PicassoImage};
use std::path::PathBuf;

/// Pack Picasso's EFS + PSP + BIOS directories. Output is a partition fragment,
/// not a full-chip image. Signed AMD firmware is never modified or re-signed.
#[derive(Parser)]
struct Args {
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
fn run(a: Args) -> amd::Result<()> {
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
fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("amdfw: {error}");
        std::process::exit(1);
    }
}
