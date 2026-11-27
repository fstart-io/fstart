//! Execute concrete platform plans. No platform/family identities are selected here.
use crate::{
    assemble::AssembledImage, board_manifest::BoardManifest, profile_source::ProfileSource,
    selection::Selection,
};
use fstart_image_build::build_plan::{BuildPlan, CompilationUnit, UnitOutput};
use fstart_image_build::files::{Blob, Files};
use fstart_image_build::plan::{BuildSelection, PlanError};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

type Artifacts = BTreeMap<String, PathBuf>;

#[cfg(test)]
#[path = "plan_executor_tests.rs"]
mod tests;

/// Caller-supplied inputs for assembling a resolved plan into an image.
pub struct AssembleRequest<'a> {
    pub release: bool,
    pub supplied: &'a BTreeMap<String, PathBuf>,
    pub payload_files: &'a [(String, String)],
    pub signing_key: Option<&'a Path>,
    pub settings: &'a fstart_image_build::options::Settings,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Resolved {
    pub plan: BuildPlan,
    /// The host directory of the blobs the plan packs, which it names
    /// `fstart_image_build::files::BLOBS_DIR`. Not part of the plan: the
    /// same blobs anywhere build the same units and image.
    #[serde(skip)]
    pub blobs: Option<PathBuf>,
}
impl Resolved {
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
        let source = crate::profile_source::load_profile(root, board)?;
        Self::try_load_with_profile(root, board, &source, selection)
    }

    pub(crate) fn try_load_with_profile(
        root: &Path,
        board: &BoardManifest,
        source: &ProfileSource,
        selection: BuildSelection,
    ) -> Result<Self, PlanError> {
        selection.validate_compiler_files()?;
        use fstart_image_build::plan::options::{BLOBS, DONOR};
        let mut selection = selection;
        if let Some(donor) = selection.options.remove(DONOR) {
            let blobs = crate::blobs::from_donor(root, source, Path::new(&donor))?;
            selection.set_option(BLOBS, blobs.to_string_lossy().into_owned());
        }
        // The plan names the blob directory `files::BLOBS_DIR`; the image
        // steps find it here.
        let blobs = selection
            .options
            .get(BLOBS)
            .map(|dir| std::path::absolute(dir).map_err(|e| format!("blob directory {dir}: {e}")))
            .transpose()?;
        let mut plan = crate::host_plan::load(root, source, board, &selection)?;
        plan.validate_compiler_selections(&selection)?;
        apply_selection_environment(&mut plan.units, &selection);
        let mut build = Self::new(root, board, source, plan)?;
        build.blobs = blobs;
        Ok(build)
    }

    pub(crate) fn new(
        root: &Path,
        board: &BoardManifest,
        source: &ProfileSource,
        mut plan: BuildPlan,
    ) -> Result<Self, String> {
        let dependency = &board
            .build_profile
            .as_ref()
            .ok_or("missing platform reference")?
            .dependency;
        for unit in &mut plan.units {
            unit.features = unit
                .features
                .iter()
                .map(|f| format!("{dependency}/{f}"))
                .chain(unit.board_features.iter().cloned())
                .chain(board.variant_features.iter().cloned())
                .collect();
            unit.features.sort();
            unit.features.dedup();
            crate::cargo_features::validate_features(
                &unit.features,
                &source.package,
                &source.metadata,
            )?;
        }
        plan.order()?;
        // The manifest arch is a discovery index; the plan is the
        // authority. Only their disagreement is an error.
        if board
            .arch
            .as_deref()
            .is_some_and(|arch| arch != plan.platform.as_str())
        {
            return Err("board manifest arch conflicts with resolved image".into());
        }
        plan.resolve_workspace_inputs(root, &board.dir)?;
        Ok(Self { plan, blobs: None })
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// JSON output for `fbuild explain`, preserving the stable top-level
    /// `{"route": "Common", "build": ...}` envelope.
    pub fn explain_json(&self) -> Result<String, String> {
        #[derive(serde::Serialize)]
        #[serde(tag = "route", rename = "Common")]
        struct Explain<'a> {
            build: &'a Resolved,
        }
        serde_json::to_string_pretty(&Explain { build: self }).map_err(|e| e.to_string())
    }

    /// The payload selected by the platform plan.
    pub fn payload(&self) -> &str {
        &self.plan.payload
    }

    /// The options the build offers, as the board's platform plan lists
    /// them from the board's option sets.
    pub fn options(&self) -> &fstart_image_build::options::Options {
        &self.plan.options
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        let digest = crate::hex_digest(Sha256::digest(self.json()?));
        Ok(root
            .join("target/fstart-build")
            .join(board)
            .join(if release { "release" } else { "debug" })
            .join(digest))
    }
    fn prepare(
        &self,
        root: &Path,
        board: &BoardManifest,
        unit: &CompilationUnit,
        artifacts: &BTreeMap<String, Artifacts>,
        release: bool,
    ) -> Result<Selection, String> {
        let mut environment = unit.environment_values.clone();
        let files = crate::compiler_inputs::collect(root, &unit.file_bindings)?;
        let mut hash = Sha256::new();
        for file in &files {
            file.hash(&mut hash)?;
        }
        for binding in &unit.bindings {
            let path = artifacts
                .get(&binding.producer)
                .and_then(|a| a.get(&binding.artifact))
                .ok_or_else(|| {
                    format!("missing artifact {}:{}", binding.producer, binding.artifact)
                })?;
            let contents =
                fs::read(path).map_err(|e| format!("artifact {}: {e}", path.display()))?;
            for bytes in [
                binding.producer.as_bytes(),
                binding.artifact.as_bytes(),
                binding.environment.as_bytes(),
                &contents,
            ] {
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
            environment.insert(binding.environment.clone(), path.display().to_string());
        }
        let directory = self
            .artifact_dir(root, &board.board, release)?
            .join(&unit.name)
            .join(crate::hex_digest(hash.finalize()));
        let records = files
            .iter()
            .map(|file| {
                let (path, record) = file.publish(&directory)?;
                environment.insert(
                    file.environment.clone(),
                    path.to_str()
                        .ok_or("non-UTF8 compiler snapshot path")?
                        .into(),
                );
                Ok(record)
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !records.is_empty() {
            fs::write(
                directory.join("compiler-inputs.json"),
                serde_json::to_vec_pretty(&records).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Selection::prepare_unit(
            root,
            board,
            unit,
            directory,
            &self.json()?,
            environment,
            &self
                .plan
                .units
                .iter()
                .flat_map(|u| {
                    u.bindings
                        .iter()
                        .map(|b| b.environment.clone())
                        .chain(u.file_bindings.iter().map(|b| b.environment.clone()))
                })
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            release,
        )
    }
    pub fn selected(&self, name: Option<&str>) -> Result<&CompilationUnit, String> {
        match name {
            Some(name) => self.plan.unit(name),
            None if self.plan.stages.len() == 1 => self.plan.unit(&self.plan.stages[0]),
            None => Err(format!(
                "select --stage from: {}",
                self.plan
                    .units
                    .iter()
                    .map(|u| u.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    /// Build exactly the selected unit's producer closure for both IDE and check.
    pub(crate) fn selection(
        &self,
        root: &Path,
        board: &BoardManifest,
        name: &str,
        release: bool,
    ) -> Result<Selection, String> {
        let mut artifacts = BTreeMap::new();
        for unit in self.plan.selection(name)? {
            let selection = self.prepare(root, board, unit, &artifacts, release)?;
            if unit.name == name {
                return Ok(selection);
            }
            artifacts.insert(unit.name.clone(), execute(root, unit, &selection, false)?);
        }
        Err("selected unit disappeared".into())
    }
    pub fn compile_selected(
        &self,
        root: &Path,
        board: &BoardManifest,
        name: &str,
        release: bool,
        checking: bool,
    ) -> Result<(), String> {
        let unit = self.selected(Some(name))?;
        let selection = self.selection(root, board, name, release)?;
        for (artifact, path) in execute(root, unit, &selection, checking)? {
            eprintln!("[fstart] {name}:{artifact}: {}", path.display());
        }
        Ok(())
    }

    pub fn check(&self, root: &Path, board: &BoardManifest, release: bool) -> Result<(), String> {
        let mut artifacts = BTreeMap::new();
        for unit in self.plan.order()? {
            let selection = self.prepare(root, board, unit, &artifacts, release)?;
            let needed = self
                .plan
                .units
                .iter()
                .any(|u| u.bindings.iter().any(|b| b.producer == unit.name));
            artifacts.insert(unit.name.clone(), execute(root, unit, &selection, !needed)?);
        }
        Ok(())
    }
    pub fn build(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
    ) -> Result<crate::build_board::BuildResult, String> {
        self.build_with_units(root, board, release, |_, _| {})
    }

    pub fn build_with_units(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
        mut linked: impl FnMut(&CompilationUnit, &Artifacts),
    ) -> Result<crate::build_board::BuildResult, String> {
        let mut artifacts = BTreeMap::new();
        for unit in self.plan.order()? {
            let selection = self.prepare(root, board, unit, &artifacts, release)?;
            let output = execute(root, unit, &selection, false)?;
            linked(unit, &output);
            artifacts.insert(unit.name.clone(), output);
        }
        let stages = self
            .plan
            .stages
            .iter()
            .map(|name| {
                let UnitOutput::Executable { load_address, .. } = self.plan.unit(name)?.output
                else {
                    return Err("non-executable stage".into());
                };
                let output = artifacts.get(name).ok_or("missing stage artifacts")?;
                Ok(fstart_image_build::StageBinary {
                    name: name.clone(),
                    path: output.get("elf").ok_or("missing ELF")?.clone(),
                    run_path: output.get("flat").ok_or("missing flat binary")?.clone(),
                    load_addr: load_address,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(crate::build_board::BuildResult { stages, artifacts })
    }
    /// Bind the caller's named external files to the inputs the plan
    /// declares, applying board-relative defaults. Returns the complete
    /// binding the image steps consume; undeclared names are rejected.
    pub fn validate_inputs(
        &self,
        board: &Path,
        supplied: &BTreeMap<String, PathBuf>,
    ) -> Result<BTreeMap<String, PathBuf>, String> {
        self.plan.validate_input_bindings()?;
        for name in supplied.keys() {
            if !self.plan.inputs.iter().any(|input| &input.name == name) {
                return Err(format!("input {name} is not declared by the selected plan"));
            }
        }
        let mut bound = BTreeMap::new();
        for input in &self.plan.inputs {
            let name = &input.name;
            let path = supplied
                .get(name)
                .cloned()
                .or_else(|| input.default.as_ref().map(|default| board.join(default)));
            let Some(path) = path else {
                return Err(format!("{name} input does not match selected plan"));
            };
            let size = fs::metadata(&path)
                .map_err(|e| format!("{name} {}: {e}", path.display()))?
                .len();
            if size == 0 || size > input.capacity {
                return Err(format!("{name} input is empty or exceeds its reservation"));
            }
            bound.insert(name.clone(), path);
        }
        Ok(bound)
    }

    /// Build the units and execute the plan's image steps. Image formats
    /// are dispatched inside `fstart-image-build`; no format identity is
    /// matched here.
    pub fn assemble(
        &self,
        root: &Path,
        board: &BoardManifest,
        request: &AssembleRequest<'_>,
    ) -> Result<AssembledImage, String> {
        let inputs = self.validate_inputs(&board.dir, request.supplied)?;
        // Bad settings fail before the units build.
        self.plan.options.seed(request.settings)?;
        let build = self.build(root, board, request.release)?;
        let smm_images: BTreeMap<String, PathBuf> = build
            .artifacts
            .iter()
            .filter_map(|(unit, outputs)| Some((unit.clone(), outputs.get("image")?.clone())))
            .collect();
        let files = fstart_image_build::files::HostFiles {
            blobs: self.blobs.clone(),
        };
        // The units are shared; the image is of these inputs only.
        let directory = self
            .artifact_dir(root, &board.board, request.release)?
            .join("image")
            .join(self.assembly_key(
                &files,
                &build.stages,
                &smm_images,
                &inputs,
                request.payload_files,
                request.signing_key,
                request.settings,
            )?);
        let outputs = fstart_image_build::image_steps::execute(
            &self.plan,
            &fstart_image_build::image_steps::ImageContext {
                files: &files,
                workspace_root: root,
                board_dir: &board.dir,
                board_name: &board.board,
                stages: &build.stages,
                smm_images: &smm_images,
                output_dir: &directory,
                inputs: &inputs,
                payload_files: request.payload_files,
                signing_key: request.signing_key,
                settings: request.settings,
            },
        )?;
        for note in &outputs.notes {
            eprintln!("[fstart] {note}");
        }
        Ok(AssembledImage {
            image: outputs.image,
            artifacts: outputs.artifacts,
            stages: build.stages,
        })
    }

    /// What an assembly of the plan reads besides it, as the name of its
    /// output directory: the blobs the plan names, the stage binaries, and
    /// the caller's inputs, payload files, signing key and settings. Two
    /// assemblies share their intermediate files only if they make the
    /// same ones.
    #[allow(clippy::too_many_arguments)]
    fn assembly_key(
        &self,
        files: &dyn Files,
        stages: &[fstart_image_build::StageBinary],
        smm_images: &BTreeMap<String, PathBuf>,
        inputs: &BTreeMap<String, PathBuf>,
        payload_files: &[(String, String)],
        signing_key: Option<&Path>,
        settings: &fstart_image_build::options::Settings,
    ) -> Result<String, String> {
        // Length-prefixed fields, each list counted: no two inputs hash alike.
        fn field(hash: &mut Sha256, bytes: &[u8]) {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        fn count(hash: &mut Sha256, n: usize) {
            hash.update((n as u64).to_le_bytes());
        }
        fn file(hash: &mut Sha256, bytes: Option<&[u8]>) {
            hash.update([u8::from(bytes.is_some())]);
            if let Some(bytes) = bytes {
                field(hash, bytes);
            }
        }
        let read = |path: &Path| fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
        let mut hash = Sha256::new();
        let plan = serde_json::to_value(&self.plan).map_err(|e| e.to_string())?;
        let mut blobs = BTreeSet::new();
        named_blobs(&plan, &mut blobs);
        count(&mut hash, blobs.len());
        for name in &blobs {
            field(&mut hash, name.as_bytes());
            if let Some(blob) = Blob::named(Path::new(name))? {
                file(&mut hash, files.blob(blob)?.as_deref());
            }
        }
        count(&mut hash, stages.len());
        for stage in stages {
            field(&mut hash, stage.name.as_bytes());
            field(&mut hash, &read(&stage.path)?);
            field(&mut hash, &read(&stage.run_path)?);
        }
        count(&mut hash, smm_images.len());
        for (unit, path) in smm_images {
            field(&mut hash, unit.as_bytes());
            field(&mut hash, &read(path)?);
        }
        count(&mut hash, inputs.len());
        for (name, path) in inputs {
            field(&mut hash, name.as_bytes());
            field(&mut hash, &read(path)?);
        }
        count(&mut hash, payload_files.len());
        for (name, path) in payload_files {
            field(&mut hash, name.as_bytes());
            field(&mut hash, &read(Path::new(path))?);
        }
        file(&mut hash, signing_key.map(read).transpose()?.as_deref());
        count(&mut hash, settings.options.len());
        for (key, value) in &settings.options {
            field(&mut hash, key.as_bytes());
            field(&mut hash, value.as_bytes());
        }
        let secure_boot = settings.secure_boot.as_ref();
        hash.update([secure_boot.map_or(0, |sb| 1 + u8::from(sb.enforce))]);
        if let Some(keys) = secure_boot.and_then(|sb| sb.keys.as_ref()) {
            field(&mut hash, &keys.pk);
            for database in [&keys.kek, &keys.db, &keys.dbx] {
                count(&mut hash, database.len());
                for file in database {
                    field(&mut hash, file);
                }
            }
        }
        Ok(crate::hex_digest(hash.finalize())[..16].into())
    }
}

/// The blobs a plan (`value`) names: its strings in the blob directory.
fn named_blobs(value: &serde_json::Value, blobs: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(path) => {
            if let Ok(Some(_)) = Blob::named(Path::new(path)) {
                blobs.insert(path.clone());
            }
        }
        serde_json::Value::Array(values) => {
            values.iter().for_each(|value| named_blobs(value, blobs));
        }
        serde_json::Value::Object(values) => {
            values.values().for_each(|value| named_blobs(value, blobs));
        }
        _ => {}
    }
}

fn execute(
    root: &Path,
    unit: &CompilationUnit,
    selection: &Selection,
    checking: bool,
) -> Result<Artifacts, String> {
    eprintln!("[fstart] building {} ({})", unit.name, unit.target);
    let output = selection
        .command(!checking)
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| e.to_string())?;
    fs::write(
        selection.directory.join(if checking {
            "cargo-check.jsonl"
        } else {
            "cargo-build.jsonl"
        }),
        &output.stdout,
    )
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("unit {} compiler failed", unit.name));
    }
    let messages = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Cargo artifact JSON: {e}"))?;
    if checking {
        return Ok(BTreeMap::new());
    }
    match &unit.output {
        UnitOutput::Executable {
            expectations,
            flat_capacity,
            ..
        } => {
            let executables = messages
                .iter()
                .filter(|v| {
                    v["reason"] == "compiler-artifact"
                        && v["target"]["name"].as_str() == Some(unit.name.as_str())
                })
                .filter_map(|v| v["executable"].as_str())
                .collect::<Vec<_>>();
            let [path] = executables.as_slice() else {
                return Err("expected exactly one current Cargo executable".into());
            };
            // The shared target directory reuses this path for every
            // unit's stage binary; keep this unit's own copy.
            let elf = selection.directory.join("stage.elf");
            fs::copy(path, &elf).map_err(|e| format!("copy {path}: {e}"))?;
            fstart_image_build::elf::validate(
                &fs::read(&elf).map_err(|e| e.to_string())?,
                expectations,
            )?;
            let flat = selection.directory.join("stage.bin");
            crate::build_board::write_flat_binary(&elf, &flat)?;
            let size = fs::metadata(&flat).map_err(|e| e.to_string())?.len();
            if size > *flat_capacity {
                return Err("flat image exceeds its load window".into());
            }
            Ok(BTreeMap::from([("elf".into(), elf), ("flat".into(), flat)]))
        }
        UnitOutput::SmmImage {
            entry_count,
            stack_size,
            coreboot_module_args,
            coreboot_header,
        } => {
            // The compiler's reported filenames are the executor's;
            // selecting, linking and imaging the SMM module is the
            // format's (fstart_image_build::smm_image).
            let filenames: Vec<&Path> = messages
                .iter()
                .filter(|v| v["reason"] == "compiler-artifact")
                .flat_map(|v| v["filenames"].as_array().into_iter().flatten())
                .filter_map(|v| v.as_str())
                .map(Path::new)
                .collect();
            let artifacts = fstart_image_build::smm_image::produce(
                filenames.into_iter(),
                &selection.target_dir(root).join(&unit.target),
                &unit.target,
                &selection.directory,
                fstart_image_build::smm_image::ImageOptions {
                    entry_count: *entry_count,
                    stack_size: *stack_size,
                    coreboot_module_args: *coreboot_module_args,
                    coreboot_header: *coreboot_header,
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(artifacts.into_iter().collect())
        }
    }
}

/// Apply build-selection environment overrides to every unit that has not
/// explicitly pinned the key (for example, SMM units pin `FSTART_LOG_LEVEL=off`
/// to satisfy their panic-free link audit).
fn apply_selection_environment(units: &mut [CompilationUnit], selection: &BuildSelection) {
    let environment = [
        ("FSTART_SMBIOS_DATE", &selection.smbios_release_date),
        ("FSTART_LOG_LEVEL", &selection.log_level),
    ];
    for (key, value) in environment {
        if let Some(value) = value {
            for unit in units.iter_mut() {
                unit.environment_values
                    .entry(key.into())
                    .or_insert_with(|| value.clone());
            }
        }
    }
}
