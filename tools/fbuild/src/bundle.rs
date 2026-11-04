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
    /// Vendor directory under boards/, for grouping boards in a UI.
    pub vendor: String,
    /// The board's human-readable name, if it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The board's CPU architecture (`package.metadata.fstart.platform`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// For a variant that is a setting of another board: that board's id
    /// and the setting, so a UI shows one board with an option.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_of: Option<OptionOf>,
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

#[derive(Serialize)]
pub struct OptionOf {
    pub board: String,
    pub option: String,
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

    fn include(
        &self,
        files: &MemoryFiles,
    ) -> Result<fstart_image_build::bundle::BundledPayload, String> {
        let dir = Path::new("/payloads/seabios");
        files.write(&dir.join("bios.bin.elf"), &self.elf)?;
        files.write(&dir.join("vgabios.bin"), &self.vgabios)?;
        files.write(&dir.join("sercon-port"), &self.sercon_port)?;
        Ok(fstart_image_build::bundle::BundledPayload {
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

pub fn export(
    out: &Path,
    revision: &str,
    board_id: Option<&str>,
    payload: Option<PayloadChoice>,
    seabios: Option<&Path>,
) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let seabios = seabios.map(SeaBios::load).transpose()?;
    let boards = match board_id {
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
        // The board's default plan says which payloads it publishes.
        let mut selection = BuildArgs::default().selection(None);
        selection.defer_firmware = true;
        let initial = ResolvedImage::load(&root, &board, selection.clone())?;
        let published = &initial.build.plan.publication_payloads;
        if published.is_empty() {
            return Err(format!("{} declares no publication payloads", board.board));
        }
        // An explicit payload for one board is built as asked; across all
        // boards, only where the platform publishes it.
        let choices: Vec<String> = match payload {
            Some(payload) if board_id.is_some() => vec![payload.as_str().to_string()],
            Some(payload) => published
                .iter()
                .filter(|p| *p == payload.as_str())
                .cloned()
                .collect(),
            None => published.clone(),
        };
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
        let donor_source = donor_required.then(|| board.donor.clone()).flatten();
        // The coreboot payload ships with SeaBIOS; its ELF slot becomes optional.
        let bundled_payload = match &seabios {
            Some(seabios) if choice == "coreboot" => Some(seabios.include(&files)?),
            _ => None,
        };
        let required: Vec<InputFile> = inputs
            .iter()
            .filter(|i| bundled_payload.as_ref().is_none_or(|p| p.input != i.name))
            .cloned()
            .collect();
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
                bundled_payload: bundled_payload.clone(),
                files: BTreeMap::new(),
            },
            files,
        };
        let mut image_error = None;
        if !donor_required
            && required.is_empty()
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
            vendor: board
                .rel_dir
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default(),
            name: board.name.clone(),
            arch: board.platform.clone(),
            option_of: board
                .option_of
                .clone()
                .map(|(board, option)| OptionOf { board, option }),
            payload: choice,
            asset,
            sha256: sha256(&bytes),
            size: bytes.len() as u64,
            inputs: required,
            bundled_payload: bundled_payload.map(|p| p.name),
            donor_required,
            donor_source,
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
