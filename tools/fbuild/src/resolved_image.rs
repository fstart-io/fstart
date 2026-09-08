//! Load and execute a concrete Rust platform plan.
//! BoardConfig host-callback boards are selected separately by board_tool.
use crate::{board_manifest::BoardManifest, payload::PayloadChoice, plan_executor::Resolved};
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
        payload: Option<PayloadChoice>,
    ) -> Result<Self, String> {
        let source = crate::profile_source::load_profile(root, board)?;
        let selection = fstart_image_build::plan::BuildSelection {
            payload: payload.map(|p| p.as_str().to_owned()),
        };
        let plan = crate::host_plan::load(root, &source, &board.variant_features, &selection)?;
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
