pub mod amd;
pub mod assemble;
pub mod build_plan;
pub mod bundle;
pub mod files;

mod coreboot_payload;
pub mod elf;
pub mod image;
pub mod inspect;
mod intel_assembly;
pub mod intel_plan;
pub mod layout;
pub mod linker;
pub mod plan;
#[cfg(not(target_arch = "wasm32"))]
pub mod smm_image;
mod summary;

use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StageBinary {
    pub name: String,
    pub path: PathBuf,
    pub run_path: PathBuf,
    pub load_addr: u64,
}
