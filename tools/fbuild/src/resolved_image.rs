//! Load and execute a concrete Rust platform plan.
//! Every board resolves through this module; the BoardConfig host-callback
//! path is retired.
use crate::{board_manifest::BoardManifest, plan_executor::Resolved};
use fstart_image_build::plan::BuildSelection;
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
#[serde(tag = "route", rename = "Common")]
pub struct ResolvedImage {
    pub(crate) build: Resolved,
    #[serde(skip)]
    _guard: crate::workspace::Guard,
}
impl ResolvedImage {
    /// Resolve while owning this checkout's orchestration lock. Nested loads
    /// on this thread share ownership; concurrent threads/processes serialize.
    pub fn load(
        root: &Path,
        board: &BoardManifest,
        selection: BuildSelection,
    ) -> Result<Self, String> {
        selection.validate_compiler_files()?;
        let source = crate::profile_source::load_profile(root, board)?;
        let mut selection = selection;
        if let Some(donor) = selection.donor.take() {
            let blobs = crate::blobs::from_donor(root, &source, std::path::Path::new(&donor))?;
            selection.blobs = Some(blobs.to_string_lossy().into_owned());
        }
        let mut plan = crate::host_plan::load(root, &source, &board.variant_features, &selection)?;
        plan.validate_compiler_selections(&selection)?;
        plan.validate_image_selection(&selection)?;
        let environment = [
            ("FSTART_SMBIOS_DATE", &selection.smbios_release_date),
            ("FSTART_LOG_LEVEL", &selection.log_level),
        ];
        for (key, value) in environment {
            if let Some(value) = value {
                for unit in &mut plan.units {
                    unit.environment_values.insert(key.into(), value.clone());
                }
            }
        }
        let build = Resolved::new(board, &source, plan)?;
        Ok(Self {
            _guard: source.guard,
            build,
        })
    }
    pub(crate) fn into_launch(
        self,
        root: &Path,
        sources: &[(&str, &Path)],
    ) -> Result<crate::launch::Session, String> {
        crate::launch::snapshot(root, self._guard, sources)
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        self.build.artifact_dir(root, board, release)
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
