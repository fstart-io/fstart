//! Load and execute a board's Rust platform plan.
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
        selection.validate_compiler_files()?;
        let source = crate::profile_source::load_profile(root, board)?;
        let mut selection = selection;
        if let Some(donor) = selection
            .options
            .remove(fstart_image_build::plan::options::DONOR)
        {
            let blobs = crate::blobs::from_donor(root, &source, std::path::Path::new(&donor))?;
            selection.set_option(
                fstart_image_build::plan::options::BLOBS,
                blobs.to_string_lossy().into_owned(),
            );
        }
        let mut plan = crate::host_plan::load(root, &source, board, &selection)?;
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
    ) -> Result<
        (
            Vec<fstart_image_build::StageBinary>,
            fstart_image_build::image_steps::ImageOutputs,
        ),
        String,
    > {
        self.build
            .assemble(root, board, release, supplied, payload_files, signing_key)
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
