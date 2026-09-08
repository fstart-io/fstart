//! Execute concrete platform plans. No platform/family identities are selected here.
use crate::{board_manifest::BoardManifest, profile_source::ProfileSource, selection::Selection};
use fstart_image_build::build_plan::{BuildPlan, CompilationUnit, UnitOutput};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

type Artifacts = BTreeMap<String, PathBuf>;

#[cfg(test)]
#[path = "plan_executor_tests.rs"]
mod tests;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Resolved {
    pub plan: BuildPlan,
}
impl Resolved {
    pub fn new(
        root: &Path,
        board: &BoardManifest,
        source: &ProfileSource,
        mut plan: BuildPlan,
    ) -> Result<Self, String> {
        if board.features != board.variant_features {
            return Err("Rust plans do not accept legacy feature recipes".into());
        }
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
        let image_target = &plan.unit(&plan.stages[0])?.target;
        if board
            .platform
            .as_deref()
            .is_some_and(|platform| platform != plan.assembly.platform.as_str())
            || board
                .target
                .as_deref()
                .is_some_and(|target| target != image_target)
        {
            return Err("board identity target/platform conflicts with resolved image".into());
        }
        plan.assembly.resolve_workspace_inputs(root, &board.dir)?;
        Ok(Self { plan })
    }
    pub fn assembler_config(&self, board: &str) -> Result<fstart_core::BoardConfig, String> {
        self.plan.assembly.config(board)
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        let digest = format!("{:x}", Sha256::digest(self.json()?));
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
        let mut hash = Sha256::new();
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
            .join(format!("{:x}", hash.finalize()));
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
                .flat_map(|u| u.bindings.iter().map(|b| b.environment.clone()))
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
    pub fn selection(
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
            artifacts.insert(
                unit.name.clone(),
                execute(root, board, unit, &selection, false)?,
            );
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
        for (artifact, path) in execute(root, board, unit, &selection, checking)? {
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
            artifacts.insert(
                unit.name.clone(),
                execute(root, board, unit, &selection, !needed)?,
            );
        }
        Ok(())
    }
    pub fn build(
        &self,
        root: &Path,
        board: &BoardManifest,
        release: bool,
    ) -> Result<crate::build_board::BuildResult, String> {
        let mut artifacts = BTreeMap::new();
        for unit in self.plan.order()? {
            let selection = self.prepare(root, board, unit, &artifacts, release)?;
            artifacts.insert(
                unit.name.clone(),
                execute(root, board, unit, &selection, false)?,
            );
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
        Ok(crate::build_board::BuildResult { stages })
    }
    pub fn validate_inputs(
        &self,
        board: &Path,
        kernel: Option<&str>,
        firmware: Option<&str>,
        fit: Option<&str>,
    ) -> Result<(), String> {
        self.plan.validate_input_bindings()?;
        for (name, supplied) in [("kernel", kernel), ("firmware", firmware), ("fit", fit)] {
            let input = self.plan.inputs.iter().find(|i| i.name == name);
            let path = supplied.map(PathBuf::from).or_else(|| {
                input
                    .and_then(|i| i.default.as_ref())
                    .map(|p| board.join(p))
            });
            match (input, path) {
                (None, None) => {}
                (Some(input), Some(path)) => {
                    let size = fs::metadata(&path)
                        .map_err(|e| format!("{name} {}: {e}", path.display()))?
                        .len();
                    if size == 0 || size > input.capacity {
                        return Err(format!("{name} input is empty or exceeds its reservation"));
                    }
                }
                _ => return Err(format!("{name} input does not match selected plan")),
            }
        }
        Ok(())
    }
}

fn execute(
    root: &Path,
    board: &BoardManifest,
    unit: &CompilationUnit,
    selection: &Selection,
    checking: bool,
) -> Result<Artifacts, String> {
    eprintln!(
        "[fstart] unit {}: {}",
        unit.name,
        selection.directory.display()
    );
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
                        && v["target"]["name"].as_str() == board.stage_bin.as_deref()
                })
                .filter_map(|v| v["executable"].as_str())
                .collect::<Vec<_>>();
            let [path] = executables.as_slice() else {
                return Err("expected exactly one current Cargo executable".into());
            };
            let elf = PathBuf::from(path);
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
            let rlibs = selection.directory.join("selected-rlibs");
            if rlibs.exists() {
                fs::remove_dir_all(&rlibs).map_err(|e| e.to_string())?;
            }
            fs::create_dir_all(&rlibs).map_err(|e| e.to_string())?;
            let target = selection.directory.join("cargo").join(&unit.target);
            for path in messages
                .iter()
                .filter(|v| v["reason"] == "compiler-artifact")
                .flat_map(|v| v["filenames"].as_array().into_iter().flatten())
                .filter_map(|v| v.as_str())
                .map(Path::new)
                .filter(|p| p.starts_with(&target) && p.extension().is_some_and(|e| e == "rlib"))
            {
                fs::copy(
                    path,
                    rlibs.join(path.file_name().ok_or("artifact has no filename")?),
                )
                .map_err(|e| e.to_string())?;
            }
            let handler = fstart_image_build::smm_image::handler_from_rlibs(
                &rlibs,
                &selection.directory.join("link"),
            )
            .map_err(|e| e.to_string())?;
            let image = selection.directory.join("image.bin");
            let header = coreboot_header.then(|| selection.directory.join("offsets.h"));
            fstart_image_build::smm_image::write_image(
                fstart_image_build::smm_image::ImageOptions {
                    entry_count: *entry_count,
                    stack_size: *stack_size,
                    coreboot_module_args: *coreboot_module_args,
                    coreboot_header: *coreboot_header,
                },
                &handler,
                &image,
                header.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            let mut artifacts = BTreeMap::from([("image".into(), image)]);
            if let Some(header) = header {
                artifacts.insert("header".into(), header);
            }
            Ok(artifacts)
        }
    }
}
