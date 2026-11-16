//! `fbuild bundle` and `fbuild compose-bundle`: publish what a build
//! compiled with its plan, and assemble a published bundle without a
//! compiler ([`fstart_image_build::bundle`]). Only explicit current-build
//! outputs and redistributable assembly inputs are published; never walk
//! target/, compiler snapshots or vendor firmware trees.
use crate::{
    board_manifest::{self, BoardManifest},
    payload::{BuildArgs, PayloadChoice},
    resolved_image::ResolvedImage,
};
use clap::ValueEnum;
use fstart_image_build::{
    build_plan::{BuildPlan, ImageOperation, InputFile},
    bundle::{
        BLOBS_DIR, Bundle, BundledPayload, Manifest, PackedDonor, Supplied, VERSION, WORKSPACE,
        sha256,
    },
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
    /// Vendor directory under boards/, for grouping boards in a UI.
    pub vendor: String,
    /// The board's human-readable name, if it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The board's ISA (`package.metadata.fstart.arch`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    pub payload: String,
    pub asset: String,
    pub sha256: String,
    pub size: u64,
    /// Files the user must supply.
    pub inputs: Vec<InputFile>,
    /// A payload shipped in the bundle (e.g. "SeaBIOS 1.17.0"); its input
    /// slot is then optional and not listed in `inputs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundled_payload: Option<String>,
    pub donor_required: bool,
    /// Where the vendor publishes a donor image this build accepts, from the
    /// board's `[package.metadata.fstart.donor]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donor_source: Option<DonorSource>,
    /// The build can seed its settings store (`--setting`): its options
    /// are in the bundle's plan.
    pub settings: bool,
    /// Compiled binaries remain available when native image placement fails.
    pub image_error: Option<String>,
    pub binaries: Vec<String>,
}

/// An official vendor download that works as this board's donor. The
/// release proxy fetches only these URLs, and the browser checks the bytes
/// against `sha256` and `size` before using them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DonorSource {
    pub vendor: String,
    /// Human-readable name, e.g. "MJ11-EC0 BIOS F02".
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// The vendor's terms that govern the download.
    pub terms: String,
    /// The vendor's download page.
    pub page: String,
    /// Caveats, e.g. that the image is for another board revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DonorSource {
    pub fn validate(&self) -> Result<(), String> {
        let https = |url: &str| url.starts_with("https://") && !url.contains(char::is_whitespace);
        if !https(&self.url) || !https(&self.terms) || !https(&self.page) {
            return Err("URLs must be https".into());
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("sha256 must be 64 hex digits".into());
        }
        if self.size == 0 || self.size > fstart_image_build::bundle::MAX_DONOR {
            return Err("size out of range".into());
        }
        Ok(())
    }
}

/// A SeaBIOS build as `ci/payloads/seabios/build.sh` leaves it.
struct SeaBios {
    name: String,
    elf: Vec<u8>,
    vgabios: Vec<u8>,
    sercon_port: Vec<u8>,
}

impl SeaBios {
    fn load(dir: &Path) -> Result<Self, String> {
        let read = |name: &str| {
            let path = dir.join(name);
            fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let version = read("version")
            .ok()
            .and_then(|v| String::from_utf8(v).ok())
            .map(|v| v.trim().trim_start_matches("rel-").to_string())
            .filter(|v| !v.is_empty());
        Ok(Self {
            name: version.map_or_else(|| "SeaBIOS".into(), |v| format!("SeaBIOS {v}")),
            elf: read("bios.bin.elf")?,
            vgabios: read("vgabios.bin")?,
            sercon_port: read("sercon-port")?,
        })
    }

    fn include(&self, files: &MemoryFiles) -> Result<BundledPayload, String> {
        let dir = Path::new("/payloads/seabios");
        files.write(&dir.join("bios.bin.elf"), &self.elf)?;
        files.write(&dir.join("vgabios.bin"), &self.vgabios)?;
        files.write(&dir.join("sercon-port"), &self.sercon_port)?;
        Ok(BundledPayload {
            name: self.name.clone(),
            input: "kernel".into(),
            path: dir.join("bios.bin.elf"),
            files: vec![
                ("vgaroms/seavgabios.bin".into(), dir.join("vgabios.bin")),
                ("etc/sercon-port".into(), dir.join("sercon-port")),
            ],
        })
    }
}

/// The board's AMD blob tables, if its image packs PSP firmware: a bundle
/// carries them instead of the firmware.
fn donor_tables(root: &Path, board: &BoardManifest) -> Result<Option<PackedDonor>, String> {
    let source = crate::profile_source::load_profile(root, board)?;
    let table = &source.package["metadata"]["fstart"];
    if table["amd"].is_null() {
        return Ok(None);
    }
    let tables =
        fstart_image_build::amd::blobs::tables(table, &source.platform["metadata"]["fstart"])?;
    Ok(Some(PackedDonor { tables }))
}

/// Export bundles of `board_id` (every board and variant without one) for
/// `payload` (every payload its platform takes without one) into the
/// empty directory `out`, with `catalog.json`.
pub fn export(
    out: &Path,
    revision: &str,
    board_id: Option<&str>,
    payload: Option<PayloadChoice>,
    seabios: Option<&Path>,
) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let seabios = seabios.map(SeaBios::load).transpose()?;
    let inventory = board_manifest::discover(&root)?;
    let boards = match board_id {
        Some(id) => vec![board_manifest::find_in(&inventory, id)?],
        None => {
            let mut all = Vec::new();
            for board in &inventory {
                let ids =
                    std::iter::once(&board.board).chain(board.variants.iter().map(|(id, _)| id));
                for id in ids {
                    all.push(board_manifest::find_in(&inventory, id)?);
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
    // Resolve every combination before starting any firmware build: a bad
    // plan must fail fast, not after an hour of work. A board publishes the
    // payloads its platform's plan takes.
    let mut builds = Vec::new();
    for board in boards {
        let packed_donor = donor_tables(&root, &board)?;
        let choices: Vec<PayloadChoice> = match payload {
            Some(payload) => vec![payload],
            None => PayloadChoice::value_variants().to_vec(),
        };
        let mut published = 0;
        for choice in choices {
            let mut selection = BuildArgs::default().selection(Some(choice));
            // An AMD plan packs the blobs its assembly extracts from the
            // user's donor.
            if packed_donor.is_some() {
                selection.set_option(fstart_image_build::plan::options::BLOBS, BLOBS_DIR);
            }
            match ResolvedImage::load(&root, &board, selection) {
                Ok(resolved) => {
                    builds.push((board.clone(), choice, resolved, packed_donor.clone()));
                    published += 1;
                }
                // Asked for explicitly: the plan must take it.
                Err(error) if payload.is_some() && board_id.is_some() => {
                    return Err(format!("{} {}: {error}", board.board, choice.as_str()));
                }
                Err(_) => {}
            }
        }
        if published == 0 && payload.is_none() {
            return Err(format!("{} takes none of fbuild's payloads", board.board));
        }
    }
    for (board, choice, resolved, packed_donor) in builds {
        catalog.builds.push(export_one(
            out,
            revision,
            &root,
            &board,
            choice.as_str(),
            &resolved,
            packed_donor,
            seabios.as_ref(),
        )?);
    }
    fs::write(
        out.join("catalog.json"),
        serde_json::to_vec_pretty(&catalog).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // one build of the export loop
fn export_one(
    out: &Path,
    revision: &str,
    root: &Path,
    board: &BoardManifest,
    choice: &str,
    resolved: &ResolvedImage,
    packed_donor: Option<PackedDonor>,
    seabios: Option<&SeaBios>,
) -> Result<Build, String> {
    let result = resolved.build(root, board, true)?;
    let plan = &resolved.build.plan;
    let board_dir = Path::new(WORKSPACE).join("boards").join(&board.rel_dir);
    let files = MemoryFiles::default();
    let mut binaries = Vec::new();
    let mut stages = result.stages.clone();
    for (unit, outputs) in &result.artifacts {
        for (kind, path) in outputs {
            let filename = path.file_name().ok_or("output has no filename")?;
            let logical = Path::new("/units").join(unit).join(filename);
            include(&files, path, &logical)?;
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
    let inputs = include_inputs(&files, root, board, &board_dir, plan)?;
    let donor_required = packed_donor.is_some();
    let donor_source = donor_required.then(|| board.donor.clone()).flatten();
    // The coreboot payload ships with SeaBIOS; its ELF slot becomes optional.
    let bundled_payload = match seabios {
        Some(seabios) if choice == "coreboot" => Some(seabios.include(&files)?),
        _ => None,
    };
    let required: Vec<InputFile> = (inputs.iter())
        .filter(|i| bundled_payload.as_ref().is_none_or(|p| p.input != i.name))
        .filter(|i| {
            i.default
                .as_ref()
                .is_none_or(|path| !files.exists(Path::new(path)))
        })
        .cloned()
        .collect();
    let mut bundle = Bundle {
        manifest: Manifest {
            format_version: VERSION,
            board: board.board.clone(),
            payload: choice.into(),
            revision: revision.into(),
            board_dir,
            plan: plan.clone(),
            stages,
            inputs: inputs.clone(),
            packed_donor,
            bundled_payload: bundled_payload.clone(),
            files: BTreeMap::new(),
        },
        files,
    };
    // A build that needs nothing from the user carries its image too.
    let mut image_error = None;
    if !donor_required && required.is_empty() && !plan.images.is_empty() {
        match bundle.assemble(&Supplied::default()) {
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
    Ok(Build {
        board: board.board.clone(),
        vendor: (board.rel_dir.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default(),
        name: board.name.clone(),
        arch: board.arch.clone(),
        payload: choice.into(),
        asset,
        sha256: sha256(&bytes),
        size: bytes.len() as u64,
        inputs: required,
        bundled_payload: bundled_payload.map(|p| p.name),
        donor_required,
        donor_source,
        settings: matches!(
            plan.options.store,
            Some(fstart_image_build::options::OptionStore::Varstore { .. })
        ),
        image_error,
        binaries,
    })
}

/// Put the workspace files the plan's image steps read into `files`, at
/// their bundle paths, and return the plan's inputs plus the board files
/// that are not redistributed (a DTB or runtime firmware the board names
/// but does not ship). Default kernel and runtime firmware bytes stay
/// external: publishing a build must not distribute board dumps.
fn include_inputs(
    files: &MemoryFiles,
    root: &Path,
    board: &BoardManifest,
    board_dir: &Path,
    plan: &BuildPlan,
) -> Result<Vec<InputFile>, String> {
    let workspace = Path::new(WORKSPACE);
    let mut inputs = plan.inputs.clone();
    // From the user's donor, at assembly.
    let donated = |path: &str| path.starts_with(BLOBS_DIR);
    for step in &plan.images {
        let ImageOperation::Ffs(assembly) = &step.operation else {
            continue;
        };
        let config = assembly.config(&board.board)?;
        match &config.microcode {
            Some(fstart_core::board::MicrocodeConfig::Intel(microcode)) => {
                include(
                    files,
                    &root.join("intel-microcode/license"),
                    Path::new("/licenses/intel-microcode"),
                )?;
                for file in &microcode.files {
                    include(
                        files,
                        &board.dir.join(file.as_str()),
                        &board_dir.join(file.as_str()),
                    )?;
                }
            }
            Some(fstart_core::board::MicrocodeConfig::Amd(microcode)) => {
                for file in microcode.files.iter().filter(|f| !donated(f)) {
                    include(
                        files,
                        &root.join(file.as_str()),
                        &workspace.join(file.as_str()),
                    )?;
                }
            }
            None => {}
        }
        for name in &config.data_assets {
            include(
                files,
                &board.dir.join(name.as_str()),
                &board_dir.join(name.as_str()),
            )?;
        }
        for asset in config.file_assets.iter().filter(|a| !donated(&a.path)) {
            include(
                files,
                &root.join(asset.path.as_str()),
                &workspace.join(asset.path.as_str()),
            )?;
        }
        let Some(payload) = &config.payload else {
            continue;
        };
        if let fstart_core::FdtSource::Override(name) = &payload.fdt {
            let path = board.dir.join(name.as_str());
            let at = board_dir.join(name.as_str());
            if path.is_file() {
                include(files, &path, &at)?;
            } else if !inputs.iter().any(|i| i.name == "dtb") {
                inputs.push(InputFile {
                    name: "dtb".into(),
                    default: Some(at.to_string_lossy().into_owned()),
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
                    (board_dir.join(firmware.file.as_str()))
                        .to_string_lossy()
                        .into_owned(),
                ),
                capacity: 16 << 20,
            });
        }
    }
    Ok(inputs)
}

/// Compiler-free native counterpart of the browser assembler.
pub fn compose(
    bundle: &Path,
    out: &Path,
    donor: Option<&Path>,
    inputs: &[String],
    key: Option<&Path>,
    settings: &[(String, String)],
) -> Result<(), String> {
    let read = |path: &Path| fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let bundle = Bundle::decode(&read(bundle)?)?;
    let donor = donor.map(read).transpose()?;
    let key = key.map(read).transpose()?;
    let mut supplied = Supplied {
        donor: donor.as_deref(),
        signing_key: key.as_deref(),
        settings: settings.to_vec(),
        ..Default::default()
    };
    for value in inputs {
        let (name, path) = value.split_once('=').ok_or("input must use NAME=PATH")?;
        if (supplied.inputs)
            .insert(name.to_string(), read(Path::new(path))?)
            .is_some()
        {
            return Err(format!("duplicate input {name}"));
        }
    }
    let outputs = bundle.assemble(&supplied)?;
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (name, bytes) in outputs {
        if Path::new(&name).components().count() != 1 {
            return Err("output must be a filename".into());
        }
        fs::write(out.join(&name), bytes).map_err(|e| e.to_string())?;
        eprintln!("[fstart] {}", out.join(name).display());
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
