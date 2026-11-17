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
    /// Print the revision the next nightly is built from (the newest master
    /// push whose CI passed), or nothing when it has a nightly already.
    Pending {
        #[arg(long, env = "GITHUB_REPOSITORY", default_value = "fstart-io/fstart")]
        repo: String,
    },
    /// Publish a built distribution as a nightly, then expire only managed
    /// nightly releases. Without a distribution, only expire.
    Nightly {
        #[arg(long, env = "GITHUB_REPOSITORY", default_value = "fstart-io/fstart")]
        repo: String,
        /// `ci/build-distribution.sh`'s output (target/distribution).
        #[arg(long, requires_all = ["revision", "run_id"])]
        distribution: Option<PathBuf>,
        /// The commit the distribution was built from.
        #[arg(long)]
        revision: Option<String>,
        /// The workflow run that built it.
        #[arg(long, env = "GITHUB_RUN_ID")]
        run_id: Option<u64>,
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
        Action::Pending { repo } => {
            if let Some(revision) = nightly::pending(&repo, &mut gh)? {
                println!("{revision}");
            }
            Ok(())
        }
        Action::Nightly {
            repo,
            distribution,
            revision,
            run_id,
        } => {
            let built = match (&distribution, &revision, run_id) {
                (Some(directory), Some(revision), Some(run_id)) => Some(nightly::Built {
                    directory,
                    revision,
                    run_id,
                }),
                (None, ..) => None,
                _ => return Err("--distribution needs --revision and --run-id".into()),
            };
            nightly::publish(&repo, time::OffsetDateTime::now_utc(), &mut gh, built)
        }
        Action::PackSdk { root, out } => sdk::archive(&root, &out),
    }
}
