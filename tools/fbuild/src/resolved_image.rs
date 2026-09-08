//! Family dispatch: an image resolves once, then projects fixed compiler units.
use crate::{
    board_manifest::BoardManifest, payload::PayloadChoice, resolved::ResolvedBuild,
    resolved_intel::ResolvedIntel,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "family", content = "build")]
pub enum ResolvedImage {
    Monolithic(ResolvedBuild),
    Intel(ResolvedIntel),
}
impl ResolvedImage {
    pub fn load(
        root: &Path,
        board: &BoardManifest,
        payload: Option<PayloadChoice>,
    ) -> Result<Self, String> {
        let source = crate::profile_source::load_profile(root, board)?;
        match source.profile["family"].as_str() {
            Some("intel-car") => {
                ResolvedIntel::from_profile_source(root, board, payload, source).map(Self::Intel)
            }
            _ => ResolvedBuild::from_profile_source(board, payload, source).map(Self::Monolithic),
        }
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        match self {
            Self::Monolithic(r) => r.artifact_dir(root, board, release),
            Self::Intel(r) => r.artifact_dir(root, board, release),
        }
    }
    pub fn assembler_config(&self, board: &str) -> Result<fstart_core::BoardConfig, String> {
        match self {
            Self::Monolithic(r) => r.assembler_config(board),
            Self::Intel(r) => r.assembler_config(board),
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
            Self::Monolithic(r) => r.validate_inputs(board, kernel, firmware, fit),
            Self::Intel(_) if kernel.is_none() && firmware.is_none() && fit.is_none() => Ok(()),
            Self::Intel(_) => Err("Intel halt/UEFI does not accept external payload inputs".into()),
        }
    }
    pub fn check(&self, root: &Path, board: &BoardManifest, release: bool) -> Result<(), String> {
        match self {
            Self::Monolithic(r) => r.check(root, board, release),
            Self::Intel(r) => crate::intel_build::check(root, board, r, release),
        }
    }
    pub fn build(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
    ) -> Result<crate::build_board::BuildResult, String> {
        match self {
            Self::Monolithic(r) => crate::resolved_build::build(root, board, r, release),
            Self::Intel(r) => crate::intel_build::build(root, board, r, release),
        }
    }
}
