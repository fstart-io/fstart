//! Publish only explicit current-build outputs and redistributable assembly
//! inputs. Never walk target/, compiler snapshots or vendor firmware trees.
use crate::{
    board_manifest,
    payload::{BuildArgs, PayloadChoice},
    resolved_image::ResolvedImage,
};
use fstart_image_build::{
    build_plan::{Assembly, InputFile},
    bundle::{Bundle, Manifest, PackedDonor, VERSION, sha256},
    files::{Files, MemoryFiles, normalize},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub struct Catalog {
    pub format_version: u32,
    pub revision: String,
    pub builds: Vec<Build>,
}
#[derive(Serialize)]
pub struct Build {
    pub board: String,
    pub payload: String,
    pub asset: String,
    pub sha256: String,
    pub size: u64,
    pub inputs: Vec<InputFile>,
    pub donor_required: bool,
    /// Compiled binaries remain available when native image placement fails.
    pub image_error: Option<String>,
    pub binaries: Vec<String>,
}

pub fn export(
    out: &Path,
    revision: &str,
    board: Option<&str>,
    payload: Option<PayloadChoice>,
) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let boards = match board {
        Some(id) => vec![board_manifest::find(&root, id)?],
        None => {
            let mut all = Vec::new();
            for board in board_manifest::discover(&root)? {
                for id in
                    std::iter::once(&board.board).chain(board.variants.iter().map(|(id, _)| id))
                {
                    all.push(board_manifest::find(&root, id)?);
                }
            }
            all
        }
    };
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    if fs::read_dir(out)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("bundle output directory must be empty".into());
    }
    let mut catalog = Catalog {
        format_version: VERSION,
        revision: revision.into(),
        builds: vec![],
    };
    // Resolve every advertised combination before starting any firmware build.
    // A bad platform capability list must fail fast, not after an hour of work.
    let mut builds = Vec::new();
    for board in boards {
        let mut selection = BuildArgs::default().selection(payload);
        selection.defer_firmware = true;
        let initial = ResolvedImage::load(&root, &board, selection.clone())?;
        let choices = if let Some(payload) = payload {
            vec![payload.as_str().to_string()]
        } else {
            initial.build.plan.publication_payloads.clone()
        };
        if choices.is_empty() {
            return Err(format!("{} declares no publication payloads", board.board));
        }
        for choice in choices {
            selection.payload = Some(choice.clone());
            let resolved = ResolvedImage::load(&root, &board, selection.clone())
                .map_err(|error| format!("{} {choice}: {error}", board.board))?;
            builds.push((board.clone(), choice, resolved));
        }
    }
    for (board, choice, resolved) in builds {
        let result = resolved.build(&root, &board, true)?;
        let plan = &resolved.build.plan;
        let board_dir = Path::new("/workspace/boards").join(&board.rel_dir);
        let files = MemoryFiles::default();
        let mut binaries = Vec::new();
        let mut stages = result.stages.clone();
        for (unit, outputs) in &result.artifacts {
            for (kind, path) in outputs {
                let filename = path.file_name().ok_or("output has no filename")?;
                let logical = Path::new("/units").join(unit).join(filename);
                files.write(
                    &logical,
                    &fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
                )?;
                binaries.push(logical.to_string_lossy().into_owned());
                if let Some(stage) = stages.iter_mut().find(|s| s.name == *unit) {
                    match kind.as_str() {
                        "elf" => stage.path = logical,
                        "flat" => stage.run_path = logical,
                        _ => {}
                    }
                }
            }
        }
        let assembly = plan.assembly.clone();
        let packed_donor = if matches!(&assembly, Assembly::Amd(image) if matches!(image.firmware, fstart_image_build::amd::assembly::Firmware::Packed { .. }))
        {
            let source = crate::profile_source::load_profile(&root, &board)?;
            let table: fstart_image_build::amd::blobs::BoardBlobs =
                serde_json::from_value(source.package["metadata"]["fstart"]["amd"].clone())
                    .map_err(|e| e.to_string())?;
            let soc = serde_json::from_value(
                source.platform["metadata"]["fstart"]["amd"][&table.soc].clone(),
            )
            .map_err(|e| e.to_string())?;
            Some(PackedDonor { soc, board: table })
        } else {
            None
        };
        let config = assembly.config(&board.board).ok();
        let mut inputs = plan.inputs.clone();
        // Default kernel/runtime firmware bytes are deliberately external:
        // publishing a build must not accidentally distribute board dumps.
        if let Some(config) = config {
            if let Some(microcode) = &config.microcode {
                match microcode {
                    fstart_core::board::MicrocodeConfig::Intel(mc) => {
                        include(
                            &files,
                            &root.join("intel-microcode/license"),
                            Path::new("/licenses/intel-microcode"),
                        )?;
                        for file in &mc.files {
                            include(
                                &files,
                                &board.dir.join(file.as_str()),
                                &board_dir.join(file.as_str()),
                            )?;
                        }
                    }
                    fstart_core::board::MicrocodeConfig::Amd(mc) => {
                        for file in &mc.files {
                            if !file.starts_with("/inputs/") {
                                include(
                                    &files,
                                    &root.join(file.as_str()),
                                    &Path::new("/workspace").join(file.as_str()),
                                )?;
                            }
                        }
                    }
                }
            }
            for name in &config.data_assets {
                include(
                    &files,
                    &board.dir.join(name.as_str()),
                    &board_dir.join(name.as_str()),
                )?;
            }
            for asset in &config.file_assets {
                if !asset.path.starts_with("/inputs/") {
                    include(
                        &files,
                        &root.join(asset.path.as_str()),
                        &Path::new("/workspace").join(asset.path.as_str()),
                    )?;
                }
            }
            if let Some(payload) = &config.payload {
                if let fstart_core::FdtSource::Override(name) = &payload.fdt {
                    let path = board.dir.join(name.as_str());
                    if path.is_file() {
                        include(&files, &path, &board_dir.join(name.as_str()))?;
                    } else {
                        inputs.push(InputFile {
                            name: "dtb".into(),
                            default: Some(
                                board_dir.join(name.as_str()).to_string_lossy().into_owned(),
                            ),
                            capacity: 1 << 20,
                        });
                    }
                }
                if let Some(firmware) = &payload.firmware
                    && !inputs.iter().any(|i| i.name == "firmware")
                {
                    inputs.push(InputFile {
                        name: "firmware".into(),
                        default: Some(
                            board_dir
                                .join(firmware.file.as_str())
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        capacity: 16 << 20,
                    });
                }
            }
        }
        let donor_required = matches!(assembly, Assembly::Amd(_));
        let mut bundle = Bundle {
            manifest: Manifest {
                format_version: VERSION,
                board: board.board.clone(),
                payload: choice.clone(),
                revision: revision.into(),
                board_dir,
                assembly,
                stages,
                inputs: inputs.clone(),
                packed_donor,
                files: BTreeMap::new(),
            },
            files,
        };
        let mut image_error = None;
        if !donor_required
            && inputs.is_empty()
            && !matches!(bundle.manifest.assembly, Assembly::Unassembled { .. })
        {
            match bundle.assemble(None, &BTreeMap::new(), None) {
                Ok(outputs) => {
                    for (name, bytes) in outputs {
                        let path = Path::new("/images").join(name);
                        bundle.files.write(&path, &bytes)?;
                        binaries.push(path.to_string_lossy().into_owned());
                    }
                }
                Err(error) => {
                    eprintln!(
                        "[fstart] {} {choice}: binaries published, image unavailable: {error}",
                        board.board
                    );
                    image_error = Some(error);
                }
            }
        }
        let bytes = bundle.encode()?;
        let asset = format!("{}-{choice}.bundle.zip", board.board);
        fs::write(out.join(&asset), &bytes).map_err(|e| e.to_string())?;
        catalog.builds.push(Build {
            board: board.board.clone(),
            payload: choice,
            asset,
            sha256: sha256(&bytes),
            size: bytes.len() as u64,
            inputs,
            donor_required,
            image_error,
            binaries,
        });
    }
    fs::write(
        out.join("catalog.json"),
        serde_json::to_vec_pretty(&catalog).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Compiler-free native counterpart of ImageAssembler::assemble.
pub fn compose(
    bundle: &Path,
    out: &Path,
    donor: Option<&Path>,
    inputs: &[String],
    key: Option<&Path>,
) -> Result<(), String> {
    let read = |path: &Path| fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let bundle = Bundle::decode(&read(bundle)?)?;
    let donor = donor.map(read).transpose()?;
    let key = key.map(read).transpose()?;
    let mut supplied = BTreeMap::new();
    for value in inputs {
        let (name, path) = value.split_once('=').ok_or("input must use NAME=PATH")?;
        if supplied
            .insert(name.to_string(), read(Path::new(path))?)
            .is_some()
        {
            return Err(format!("duplicate input {name}"));
        }
    }
    let outputs = bundle.assemble(donor.as_deref(), &supplied, key.as_deref())?;
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (name, bytes) in outputs {
        if Path::new(&name).components().count() != 1 {
            return Err("output must be a filename".into());
        }
        fs::write(out.join(name), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn include(files: &MemoryFiles, source: &Path, destination: &Path) -> Result<(), String> {
    let destination: PathBuf = normalize(destination);
    files.write(
        &destination,
        &fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?,
    )
}
