//! CLI for generating standalone fstart SMM image artifacts.

use std::path::PathBuf;

use clap::Parser;
use fstart_smm_image::{write_image, ImageOptions};

#[derive(Debug, Parser)]
#[command(
    name = "fstart-smm-image",
    about = "Generate a standalone fstart PIC SMM image"
)]
struct Args {
    /// Number of precompiled SMM entry stubs to include.
    #[arg(long)]
    entries: u16,
    /// Per-CPU SMM stack size. Decimal or 0x-prefixed hex.
    #[arg(long, value_parser = parse_u32)]
    stack_size: u32,
    /// Output native SMM image path.
    #[arg(long)]
    out: PathBuf,
    /// Optional generated coreboot offsets header path.
    #[arg(long)]
    coreboot_header: Option<PathBuf>,
    /// Include coreboot-style module argument storage in the handler/data region.
    #[arg(long, default_value_t = false)]
    coreboot_module_args: bool,
}

fn main() {
    let args = Args::parse();
    let options = ImageOptions {
        entry_count: args.entries,
        stack_size: args.stack_size,
        coreboot_module_args: args.coreboot_module_args,
        coreboot_header: args.coreboot_header.is_some(),
    };

    match write_image(options, &args.out, args.coreboot_header.as_deref()) {
        Ok(built) => {
            eprintln!(
                "[fstart-smm-image] wrote {} ({} bytes, {} entries)",
                args.out.display(),
                built.image.len(),
                args.entries
            );
            if let Some(path) = args.coreboot_header {
                eprintln!("[fstart-smm-image] wrote {}", path.display());
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn parse_u32(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|e| e.to_string())
    }
}
