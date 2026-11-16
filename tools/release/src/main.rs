//! Trusted CI-side tooling. Downloaded artifacts are data, never executables.
mod nightly;

use clap::{Parser, Subcommand};
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(about = "Publish verified CI artifacts as nightly releases")]
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
    }
}
