//! Load and execute a board's Rust platform plan.
use crate::{board_manifest::BoardManifest, plan_executor::Resolved};
use fstart_image_build::plan::{BuildSelection, PlanError};
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
        Self::try_load(root, board, selection).map_err(|e| e.to_string())
    }

    pub fn try_load(
        root: &Path,
        board: &BoardManifest,
        selection: BuildSelection,
    ) -> Result<Self, PlanError> {
        selection.validate_compiler_files()?;
        let source = crate::profile_source::load_profile(root, board)?;
        use fstart_image_build::plan::options::{BLOBS, DONOR};
        let mut selection = selection;
        if let Some(donor) = selection.options.remove(DONOR) {
            let blobs = crate::blobs::from_donor(root, &source, Path::new(&donor))?;
            selection.set_option(BLOBS, blobs.to_string_lossy().into_owned());
        }
        // The plan names the blob directory `files::BLOBS_DIR`; the image
        // steps find it here.
        let blobs = selection
            .options
            .get(BLOBS)
            .map(|dir| std::path::absolute(dir).map_err(|e| format!("blob directory {dir}: {e}")))
            .transpose()?;
        let mut plan = crate::host_plan::load(root, &source, board, &selection)?;
        plan.validate_compiler_selections(&selection)?;
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
        let mut build = Resolved::new(root, board, &source, plan)?;
        build.blobs = blobs;
        Ok(Self { build })
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    /// The payload selected by the platform plan.
    pub fn payload(&self) -> &str {
        &self.build.plan.payload
    }
    /// The options the build offers, as the board's platform plan lists
    /// them from the board's option sets.
    pub fn options(&self) -> &fstart_image_build::options::Options {
        &self.build.plan.options
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        self.build.artifact_dir(root, board, release)
    }
    pub fn validate_inputs(
        &self,
        board: &Path,
        supplied: &std::collections::BTreeMap<String, PathBuf>,
    ) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
        self.build.validate_inputs(board, supplied)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
        supplied: &std::collections::BTreeMap<String, PathBuf>,
        payload_files: &[(String, String)],
        signing_key: Option<&Path>,
        settings: &fstart_image_build::options::Settings,
    ) -> Result<
        (
            Vec<fstart_image_build::StageBinary>,
            fstart_image_build::image_steps::ImageOutputs,
        ),
        String,
    > {
        self.build.assemble(
            root,
            board,
            release,
            supplied,
            payload_files,
            signing_key,
            settings,
        )
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

    /// Report each unit whose linked artifacts are ready, even if a later unit fails.
    pub fn build_with_units(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
        linked: impl FnMut(
            &fstart_image_build::build_plan::CompilationUnit,
            &std::collections::BTreeMap<String, PathBuf>,
        ),
    ) -> Result<crate::build_board::BuildResult, String> {
        self.build.build_with_units(root, board, release, linked)
    }
}
