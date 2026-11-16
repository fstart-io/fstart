pub mod amd;
pub mod assemble;
pub mod boot_test;
pub mod build_plan;
pub mod bundle;
pub mod files;

mod coreboot_payload;
pub mod donor;
pub mod elf;
mod elf_flat;
mod elf_payload;
pub mod image;
pub mod image_steps;
pub mod inspect;
mod intel_assembly;
pub mod intel_plan;
pub mod layout;
pub mod linker;
pub mod options;
pub mod plan;
pub mod run;
pub mod smm_image;
mod summary;
pub mod x86_flash;

use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StageBinary {
    pub name: String,
    pub path: PathBuf,
    pub run_path: PathBuf,
    pub load_addr: u64,
}
