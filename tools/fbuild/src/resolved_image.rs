//! Explicit migration boundary: concrete Rust plans or the legacy metadata route.
use crate::{
    board_manifest::BoardManifest, payload::PayloadChoice, plan_executor::Resolved,
    resolved::ResolvedBuild,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "route", content = "build")]
pub enum ResolvedImage {
    Legacy(ResolvedBuild),
    Common(Resolved),
}
impl ResolvedImage {
    pub fn load(
        root: &Path,
        board: &BoardManifest,
        payload: Option<PayloadChoice>,
    ) -> Result<Self, String> {
        let source = crate::profile_source::load_profile(root, board)?;
        if board
            .build_profile
            .as_ref()
            .is_some_and(|p| p.name == "rust")
        {
            let selection = fstart_image_build::plan::BuildSelection {
                payload: payload.map(|p| p.as_str().to_owned()),
            };
            let plan = crate::host_plan::load(root, &source, &board.variant_features, &selection)?;
            Resolved::new(root, board, &source, plan).map(Self::Common)
        } else {
            ResolvedBuild::from_profile_source(board, payload, source).map(Self::Legacy)
        }
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        match self {
            Self::Legacy(r) => r.artifact_dir(root, board, release),
            Self::Common(r) => r.artifact_dir(root, board, release),
        }
    }
    pub fn bootstrap_bindings(
        &self,
    ) -> Option<&[(String, fstart_image_build::build_plan::BootstrapRole)]> {
        match self {
            Self::Common(plan) => Some(&plan.plan.assembly.bootstrap),
            Self::Legacy(_) => None,
        }
    }

    pub fn assembler_config(&self, board: &str) -> Result<fstart_core::BoardConfig, String> {
        match self {
            Self::Legacy(r) => r.assembler_config(board),
            Self::Common(r) => r.assembler_config(board),
        }
    }
    pub fn validate_inputs(
        &self,
        board: &Path,
        kernel: Option<&str>,
        firmware: Option<&str>,
        fit: Option<&str>,
    ) -> Result<(), String> {
        match self {
            Self::Legacy(r) => r.validate_inputs(board, kernel, firmware, fit),
            Self::Common(r) => r.validate_inputs(board, kernel, firmware, fit),
        }
    }
    pub fn compile_selected(
        &self,
        root: &Path,
        board: &BoardManifest,
        name: &str,
        release: bool,
        checking: bool,
    ) -> Result<(), String> {
        match self {
            Self::Common(plan) => plan.compile_selected(root, board, name, release, checking),
            Self::Legacy(_) if name != "stage" => {
                Err("legacy metadata profile has only unit 'stage'".into())
            }
            Self::Legacy(_) if checking => self.check(root, board, release),
            Self::Legacy(_) => self.build(root, board, release).map(|_| ()),
        }
    }

    pub fn check(&self, root: &Path, board: &BoardManifest, release: bool) -> Result<(), String> {
        match self {
            Self::Legacy(r) => r.check(root, board, release),
            Self::Common(r) => r.check(root, board, release),
        }
    }
    pub fn build(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
    ) -> Result<crate::build_board::BuildResult, String> {
        match self {
            Self::Legacy(r) => crate::resolved_build::build(root, board, r, release),
            Self::Common(r) => r.build(root, board, release),
        }
    }
}
