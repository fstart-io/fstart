// SPDX-License-Identifier: MIT OR Apache-2.0
//! Build declarative APCBs; JSON export is a lossless diagnostic interchange.
use clap::{Parser, Subcommand};
use fstart_image_build::amd::{Result, apcb_source, apcb_toml};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Build or inspect typed APCB v2/v3 configuration for PSP/ABL")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export a standalone APCB as lossless, typed diagnostic JSON.
    Decode { input: PathBuf, output: PathBuf },
    /// Build a named payload from a board's declarative TOML source.
    #[command(alias = "build-toml")]
    Build {
        input: PathBuf,
        output: PathBuf,
        /// Firmware defaults/ABI template, normally supplied by the board build.
        #[arg(long)]
        template: PathBuf,
        /// Optional shared profile defaults (used by SP3 boards).
        #[arg(long)]
        tables: Option<PathBuf>,
        #[arg(long, default_value = "board")]
        blob: String,
    },
    /// Generate an editor JSON Schema for semantic board policy.
    #[command(alias = "toml-schema")]
    Schema { output: PathBuf },
    /// Rebuild a diagnostic JSON export without interpreting it as board policy.
    BuildJson { input: PathBuf, output: PathBuf },
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn run(args: Args) -> Result<()> {
    let (path, bytes) = match args.command {
        Command::Decode { input, output } => {
            let source = apcb_source::decode(&read(&input)?)?;
            let mut bytes = serde_json::to_vec_pretty(&source).map_err(|e| e.to_string())?;
            bytes.push(b'\n');
            (output, bytes)
        }
        Command::Schema { output } => {
            let schema = apcb_toml::schema();
            let mut bytes = serde_json::to_vec_pretty(&schema).map_err(|e| e.to_string())?;
            bytes.push(b'\n');
            (output, bytes)
        }
        Command::Build {
            input,
            output,
            tables,
            template,
            blob,
        } => {
            let text = String::from_utf8(read(&input)?).map_err(|e| e.to_string())?;
            let shared = tables
                .map(|path| {
                    read(&path).and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
                })
                .transpose()?
                .unwrap_or_else(|| "schema=1\n[tables]\n".into());
            let template = String::from_utf8(read(&template)?).map_err(|e| e.to_string())?;
            (
                output,
                apcb_toml::build_blob(&text, &template, &shared, &blob)?,
            )
        }
        Command::BuildJson { input, output } => {
            let source = serde_json::from_slice(&read(&input)?).map_err(|e| e.to_string())?;
            (
                output,
                apcb_source::build(&source, &apcb_source::Tables::new())?,
            )
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
        eprintln!("apcb-source: {error}");
        std::process::exit(1);
    }
}
