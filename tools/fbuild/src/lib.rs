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
