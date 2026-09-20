//! Load and execute a concrete Rust platform plan.
//! Every board resolves through this module; the BoardConfig host-callback
//! path is retired.
use crate::{board_manifest::BoardManifest, plan_executor::Resolved};
use fstart_image_build::plan::BuildSelection;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "route", rename = "Common")]
pub struct ResolvedImage {
    pub(crate) build: Resolved,
}
impl ResolvedImage {
    pub fn load(
        root: &Path,
        board: &BoardManifest,
        selection: BuildSelection,
    ) -> Result<Self, String> {
        let source = crate::profile_source::load_profile(root, board)?;
        let mut plan = crate::host_plan::load(root, &source, &board.variant_features, &selection)?;
        if let Some(date) = &selection.smbios_release_date {
            for unit in &mut plan.units {
                unit.environment_values
                    .insert("FSTART_SMBIOS_DATE".into(), date.clone());
            }
        }
        Ok(Self {
            build: Resolved::new(root, board, &source, plan)?,
        })
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        self.build.artifact_dir(root, board, release)
    }
    pub fn bootstrap_bindings(
        &self,
    ) -> Option<&[(String, fstart_image_build::build_plan::BootstrapRole)]> {
        Some(&self.build.plan.assembly.bootstrap)
    }
    pub fn assembler_config(&self, board: &str) -> Result<fstart_core::BoardConfig, String> {
        self.build.assembler_config(board)
    }
    pub fn validate_inputs(
        &self,
        board: &Path,
        kernel: Option<&str>,
        firmware: Option<&str>,
        fit: Option<&str>,
    ) -> Result<(), String> {
        self.build.validate_inputs(board, kernel, firmware, fit)
    }
    pub fn compile_selected(
        &self,
        root: &Path,
        board: &BoardManifest,
        name: &str,
        release: bool,
        checking: bool,
    ) -> Result<(), String> {
        self.build
            .compile_selected(root, board, name, release, checking)
    }
    pub fn check(&self, root: &Path, board: &BoardManifest, release: bool) -> Result<(), String> {
        self.build.check(root, board, release)
    }
    pub fn build(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
    ) -> Result<crate::build_board::BuildResult, String> {
        self.build.build(root, board, release)
    }
}
