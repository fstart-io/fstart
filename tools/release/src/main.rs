//! Trusted CI-side tooling. Downloaded artifacts are data, never executables.
mod nightly;
mod sdk;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::Command};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(about = "Publish verified CI artifacts and package the browser SDK")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Promote successful master CI and expire only managed nightly releases.
    Nightly {
        #[arg(long, env = "GITHUB_REPOSITORY", default_value = "fstart-io/fstart")]
        repo: String,
    },
    /// ZIP/DEFLATE the Pages SDK, excluding test-only Node bindings.
    PackSdk {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh").args(args).output()?;
    if !output.status.success() {
        return Err(format!(
            "gh failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Nightly { repo } => {
            nightly::publish(&repo, time::OffsetDateTime::now_utc(), &mut gh)
        }
        Action::PackSdk { root, out } => sdk::archive(&root, &out),
    }
}
