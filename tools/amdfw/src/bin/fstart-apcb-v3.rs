// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed APCB v3 import/export; the existing fstart-apcb remains a v2 SPD editor.
use clap::{Parser, Subcommand};
use fstart_image_build::amd::{Result, apcb_toml, apcb_v3};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Decode or build typed APCB v3 configuration for PSP/ABL")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export a standalone APCB binary as lossless, typed JSON (no raw fallback).
    Decode { input: PathBuf, output: PathBuf },
    /// Build an experimental typed TOML payload; does not change board defaults.
    BuildToml {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        tables: PathBuf,
        #[arg(long, default_value = "board")]
        blob: String,
    },
    /// Generate an editor JSON Schema for TOML boards or shared tables.
    TomlSchema {
        output: PathBuf,
        #[arg(long)]
        tables: bool,
    },
    /// Build one APCB from typed JSON, without a donor binary or template.
    Build {
        input: PathBuf,
        output: PathBuf,
        /// Shared typed electrical tables, when the source uses table references.
        #[arg(long)]
        tables: Option<PathBuf>,
        /// Select a named payload from a board's schema-1 source set.
        #[arg(long)]
        blob: Option<String>,
    },
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn run(args: Args) -> Result<()> {
    let (path, bytes) = match args.command {
        Command::Decode { input, output } => {
            let source = apcb_v3::decode(&read(&input)?)?;
            let mut bytes = serde_json::to_vec_pretty(&source).map_err(|e| e.to_string())?;
            bytes.push(b'\n');
            (output, bytes)
        }
        Command::TomlSchema { output, tables } => {
            let schema = if tables {
                apcb_toml::tables_schema()
            } else {
                apcb_toml::schema()
            };
            let mut bytes = serde_json::to_vec_pretty(&schema).map_err(|e| e.to_string())?;
            bytes.push(b'\n');
            (output, bytes)
        }
        Command::BuildToml {
            input,
            output,
            tables,
            blob,
        } => {
            let text = String::from_utf8(read(&input)?).map_err(|e| e.to_string())?;
            let shared = String::from_utf8(read(&tables)?).map_err(|e| e.to_string())?;
            // Validate the complete source set, then select by its semantic name.
            let config = apcb_toml::Configuration::parse(&text)?;
            let source = config
                .blobs
                .iter()
                .find(|b| b.name == blob)
                .ok_or_else(|| format!("no APCB named {blob}"))?;
            let binding = source.bindings.first().ok_or("APCB has no bindings")?;
            let selector = binding.selector(config.generation);
            let built = apcb_toml::compile(&text, &shared)?;
            let payload = built
                .into_iter()
                .find(|r| r.selector == selector)
                .ok_or("APCB binding was not compiled")?;
            (output, payload.bytes)
        }
        Command::Build {
            input,
            output,
            tables,
            blob,
        } => {
            let source = serde_json::from_slice(&read(&input)?).map_err(|e| e.to_string())?;
            let source = if let Some(name) = blob {
                let sources: apcb_v3::Sources =
                    serde_json::from_value(source).map_err(|e| e.to_string())?;
                if sources.schema != 1 {
                    return Err("expected APCB source schema 1".into());
                }
                let mut matches = sources
                    .blobs
                    .into_iter()
                    .filter(|source| source.name == name);
                let source = matches
                    .next()
                    .ok_or_else(|| format!("no APCB named {name}"))?;
                if matches.next().is_some() {
                    return Err(format!("ambiguous APCB name {name}"));
                }
                source.apcb
            } else {
                source
            };
            let tables = tables
                .map(|p| serde_json::from_slice(&read(&p)?).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            (output, apcb_v3::build(&source, &tables)?)
        }
    };
    // Compile and validate before creating anything; never clobber an input.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .write_all(&bytes)
        .map_err(|e| e.to_string())
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("apcb-v3: {error}");
        std::process::exit(1);
    }
}
