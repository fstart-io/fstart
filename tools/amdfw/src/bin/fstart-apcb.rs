// SPDX-License-Identifier: MIT OR Apache-2.0
use clap::Parser;
use fstart_image_build::amd::{Result, apcb};
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Inject Picasso APCB v2 RAM-ID wiring and per-channel DDR4 SPDs")]
struct Args {
    #[arg(long)]
    template: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Four NUMBER:MUX:BANK_CONTROL triples in bit order; decimal or 0x hex.
    #[arg(long, required = true, value_parser = gpio)]
    gpio: Vec<[u8; 3]>,
    #[arg(long)]
    spd_0_0: Option<PathBuf>,
    #[arg(long)]
    spd_0_1: Option<PathBuf>,
    #[arg(long)]
    spd_1_0: Option<PathBuf>,
    #[arg(long)]
    spd_1_1: Option<PathBuf>,
    #[arg(long)]
    hex: bool,
    #[arg(long)]
    strip_manufacturer: bool,
}
fn gpio(value: &str) -> Result<[u8; 3]> {
    let bytes: Vec<_> = value
        .split(':')
        .map(|s| {
            let (s, base) = s.strip_prefix("0x").map_or((s, 10), |s| (s, 16));
            u8::from_str_radix(s, base).map_err(|e| e.to_string())
        })
        .collect::<Result<_>>()?;
    bytes
        .try_into()
        .map_err(|_| "expected NUMBER:MUX:BANK_CONTROL".into())
}
fn run(a: Args) -> Result<()> {
    let gpio = a
        .gpio
        .try_into()
        .map_err(|_| "exactly four --gpio triples are required")?;
    let template = std::fs::read(a.template).map_err(|e| e.to_string())?;
    let mut spds = [[None; 2]; 2];
    for (i, path) in [a.spd_0_0, a.spd_0_1, a.spd_1_0, a.spd_1_1]
        .into_iter()
        .enumerate()
    {
        if let Some(path) = path {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            spds[i / 2][i % 2] = Some(if a.hex {
                apcb::parse_spd_hex(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)?
            } else {
                bytes
                    .try_into()
                    .map_err(|_| "binary SPD must be 512 bytes")?
            });
        }
    }
    let borrowed = spds
        .each_ref()
        .map(|row| row.each_ref().map(Option::as_ref));
    let output = apcb::inject(&template, gpio, borrowed, a.strip_manufacturer)?;
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(a.output)
        .map_err(|e| e.to_string())?
        .write_all(&output)
        .map_err(|e| e.to_string())
}
fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("apcb: {error}");
        std::process::exit(1);
    }
}
