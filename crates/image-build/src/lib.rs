pub mod assemble;
pub mod image;
pub mod inspect;
pub mod intel_plan;
pub mod layout;
pub mod plan;
pub mod smm_image;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct StageBinary {
    pub name: String,
    pub path: PathBuf,
    pub run_path: PathBuf,
    pub load_addr: u64,
}
