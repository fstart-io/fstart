pub mod acpi_check;
pub mod assemble;
pub mod blobs;
pub mod board_manifest;
pub mod board_tool;
pub mod boot_test;
pub mod build_board;
pub mod bundle;
mod cargo_features;
pub mod checks;
mod compiler_inputs;
mod host_plan;
pub mod ide;
mod ide_graph;
#[cfg(test)]
mod intel_linker_tests;
pub mod payload;
pub mod plan_executor;
mod profile_source;
pub mod qemu;
#[cfg(test)]
mod qemu_golden_tests;
mod selection;
pub mod shards;
pub mod size_report;
pub mod sizes;
pub mod swtpm;
pub mod tpm_log;

pub(crate) fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Process-wide per-directory lock.
///
/// Used to serialize concurrent Cargo invocations + artifact extraction against
/// the same `--target-dir` (so parallel boards sharing a target directory never
/// clobber each other's outputs), and to serialize repeated occurrences of the
/// same board output directory (`target/fstart-build/<board>`,
/// `target/fstart-acpi/<board>`).
pub fn path_lock(dir: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};

    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let _ = std::fs::create_dir_all(dir);
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let map = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    map.lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(crate) fn target_dir_lock(dir: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    path_lock(dir)
}

/// Default worker count for parallel board verification commands.
///
/// Capped at 4 so large local machines do not spawn dozens of concurrent
/// Cargo/LTO processes while still saturating typical 4-core CI runners.
pub fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .clamp(1, 4)
}
