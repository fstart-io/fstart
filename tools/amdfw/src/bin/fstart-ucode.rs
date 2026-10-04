// SPDX-License-Identifier: MIT OR Apache-2.0
//! Assemble unchanged external flat patches. Never authenticates or loads them.
use clap::Parser;
use fstart_driver_amd::picasso::cpu::microcode::{Bundle, MAX_PATCHES, PATCH_BYTES, Patch};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(about = "Concatenate trusted external Picasso microcode into a checked compiler input")]
struct Args {
    /// Repeat HEX_CPUID:PATH for the exact stepping of each flat vendor input.
    /// The caller establishes provenance, authentication and security freshness.
    #[arg(long, required = true, value_parser = input)]
    patch: Vec<(u32, PathBuf)>,
    /// Refuses overwrite. No vendor bytes are added to source control.
    #[arg(long)]
    output: PathBuf,
}
fn input(value: &str) -> Result<(u32, PathBuf), String> {
    let (signature, path) = value.split_once(':').ok_or("expected HEX_CPUID:PATH")?;
    if path.is_empty() {
        return Err("empty patch path".into());
    }
    let signature =
        u32::from_str_radix(signature.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    Ok((signature, path.into()))
}
fn run(args: Args) -> Result<(), String> {
    if args.patch.len() > MAX_PATCHES {
        return Err(format!("at most {MAX_PATCHES} flat inputs"));
    }
    // Canonical order is input identity, not CLI ordering or a best-version rule.
    let mut inputs = args.patch;
    inputs.sort_by_key(|(signature, _)| *signature);
    let mut bundle = Vec::new();
    let mut report = Vec::new();
    for (signature, path) in inputs {
        let mut bytes = Vec::with_capacity(PATCH_BYTES + 1);
        std::fs::File::open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .take(PATCH_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let patch =
            Patch::parse(&bytes, signature).map_err(|e| format!("{}: {e:?}", path.display()))?;
        report.push(format!(
            "cpuid={signature:08x} revision={:08x} sha256={:x} source={}",
            patch.revision(),
            Sha256::digest(&bytes),
            path.display()
        ));
        bundle.extend_from_slice(&bytes);
    }
    Bundle::parse(&bundle).map_err(|e| format!("bundle: {e:?}"))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)
        .map_err(|e| format!("{}: {e}", args.output.display()))?;
    // A write failure may leave a partial file; it is never implicitly reused or
    // overwritten. Compilation/runtime validators independently reject it.
    output.write_all(&bundle).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    for line in report {
        println!("{line}");
    }
    println!(
        "bundle bytes={} sha256={:x} output={}",
        bundle.len(),
        Sha256::digest(&bundle),
        args.output.display()
    );
    Ok(())
}
fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("ucode: {error}");
        std::process::exit(1);
    }
}
