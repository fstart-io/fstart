//! `fbuild bundle` and `fbuild compose-bundle`: publish what a build
//! compiled with its plan, and assemble a published bundle without a
//! compiler ([`fstart_image_build::bundle`]). Only explicit current-build
//! outputs and redistributable assembly inputs are published; never walk
//! target/, compiler snapshots or vendor firmware trees.
use crate::{
    board_manifest::{self, BoardManifest},
    payload::{PayloadChoice, PlanArgs},
    resolved_image::ResolvedImage,
};
use fstart_image_build::{
    build_plan::{BuildPlan, ImageOperation, InputFile},
    bundle::{
        BLOBS_DIR, Bundle, BundledPayload, Manifest, PackedDonor, Supplied, VERSION, WORKSPACE,
        sha256,
    },
    files::{Files, MemoryFiles, normalize},
    plan::PlanError,
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
fn donor_tables(
    source: &crate::profile_source::ProfileSource,
) -> Result<Option<PackedDonor>, String> {
    let table = &source.package["metadata"]["fstart"];
    if table["amd"].is_null() {
        return Ok(None);
    }
    let tables =
        fstart_image_build::amd::blobs::tables(table, &source.platform["metadata"]["fstart"])?;
    Ok(Some(PackedDonor { tables }))
}

/// What a board publishes without `--payload`: of each group, the first
/// payload its platform's plan takes. UEFI is CrabEFI with its setup UI,
/// or plain CrabEFI where the plan has no UI build. `halt` and
/// `uefi-basic` are for development and are not published.
const PUBLISHED: &[&[PayloadChoice]] = &[
    &[PayloadChoice::UefiUi, PayloadChoice::Uefi],
    &[PayloadChoice::Linux],
    &[PayloadChoice::Coreboot],
    &[PayloadChoice::Elf],
];

/// The smallest kernel slot a published Linux build has: a usable kernel
/// with its initramfs does not fit in less.
const LINUX_MIN: u64 = 1536 << 10;

fn fits_a_kernel(resolved: &ResolvedImage) -> bool {
    (resolved.build.plan.inputs.iter())
        .find(|input| input.name == "kernel")
        .is_none_or(|kernel| kernel.capacity >= LINUX_MIN)
}

/// Export bundles of `board_id` (every board and variant without one) for
/// `payload` (without one, the payloads a board publishes: [`PUBLISHED`])
/// into the empty directory `out`, with `catalog.json`.
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
    // plan must fail fast, not after an hour of work.
    let mut builds = Vec::new();
    for board in boards {
        let source = crate::profile_source::load_profile(&root, &board)?;
        let packed_donor = donor_tables(&source)?;
        let selected = select_payloads_for_board(
            &board.board,
            board_id.is_some(),
            payload,
            |choice| {
                let mut selection = PlanArgs::default().selection(Some(choice));
                // An AMD plan packs the blobs its assembly extracts from the
                // user's donor.
                if packed_donor.is_some() {
                    selection.set_option(fstart_image_build::plan::options::BLOBS, BLOBS_DIR);
                }
                ResolvedImage::try_load_with_profile(&root, &board, &source, selection)
            },
            fits_a_kernel,
        )?;
        if selected.is_empty() && payload.is_none() {
            eprintln!("[fstart] {}: no published payload, skipped", board.board);
        }
        for (choice, resolved) in selected {
            builds.push((board.clone(), choice, resolved, packed_donor.clone()));
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
    // The stock SeaBIOS package does not fit alongside firmware on 512 KiB
    // flash. Keep the external ELF slot there for smaller custom payloads.
    let fits_seabios = inputs
        .iter()
        .any(|input| input.name == "kernel" && input.capacity > 512 * 1024);
    let bundled_payload = match seabios {
        Some(seabios) if choice == "coreboot" && fits_seabios => Some(seabios.include(&files)?),
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
    // A build that needs nothing from the user must carry its image too.
    if !donor_required && required.is_empty() && !plan.images.is_empty() {
        let outputs = bundle.assemble(&Supplied::default()).map_err(|error| {
            format!(
                "{} {choice}: bundle image assembly failed: {error}",
                board.board
            )
        })?;
        for (name, bytes) in outputs {
            let path = Path::new("/images").join(name);
            bundle.files.write(&path, &bytes)?;
            binaries.push(path.to_string_lossy().into_owned());
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
    let donated = |path: &str| Path::new(path).starts_with(BLOBS_DIR);
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
    intel_flash: Option<&Path>,
    inputs: &[String],
    key: Option<&Path>,
    settings: &fstart_image_build::options::Settings,
) -> Result<(), String> {
    let read = |path: &Path| fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let bundle = Bundle::decode(&read(bundle)?)?;
    let donor = donor.map(crate::blobs::read_donor).transpose()?;
    let intel_flash = intel_flash.map(crate::blobs::read_donor).transpose()?;
    let key = key.map(read).transpose()?;
    let mut supplied = Supplied {
        intel_flash: intel_flash.as_deref(),
        donor: donor.as_deref(),
        signing_key: key.as_deref(),
        settings: settings.clone(),
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
    write_outputs(out, &outputs, intel_flash.is_some())
}

fn write_outputs(
    out: &Path,
    outputs: &std::collections::BTreeMap<String, Vec<u8>>,
    no_clobber: bool,
) -> Result<(), String> {
    use std::io::{ErrorKind, Write};
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for name in outputs.keys() {
        if !matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [std::path::Component::Normal(_)]
        ) {
            return Err("output must be a filename".into());
        }
        if no_clobber {
            let path = out.join(name);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == ErrorKind::NotFound => (),
                Err(e) => return Err(format!("{}: {e}", path.display())),
                Ok(_) => {
                    return Err(format!(
                        "refusing to overwrite {} with --intel-flash",
                        path.display()
                    ));
                }
            }
        }
    }
    for (name, bytes) in outputs {
        let path = out.join(name);
        if no_clobber {
            fs::File::create_new(&path).and_then(|mut file| file.write_all(bytes))
        } else {
            fs::write(&path, bytes)
        }
        .map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("[fstart] {}", path.display());
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

/// Resolve the published payload choices for one board.
///
/// Only [`PlanError::UnsupportedPayload`] allows skipping a board or falling
/// back within a [`PUBLISHED`] group; any other failure aborts export before
/// any firmware build starts.
fn select_payloads_for_board<T>(
    board: &str,
    explicit_board: bool,
    payload: Option<PayloadChoice>,
    mut resolve: impl FnMut(PayloadChoice) -> Result<T, PlanError>,
    fits_kernel: impl Fn(&T) -> bool,
) -> Result<Vec<(PayloadChoice, T)>, String> {
    if let Some(choice) = payload {
        return match resolve(choice) {
            Ok(resolved) => Ok(vec![(choice, resolved)]),
            Err(PlanError::UnsupportedPayload(_)) if !explicit_board => Ok(Vec::new()),
            Err(error) => Err(format!("{board} {}: {error}", choice.as_str())),
        };
    }
    let mut selected = Vec::new();
    for group in PUBLISHED {
        let mut picked = None;
        for &choice in *group {
            match resolve(choice) {
                Ok(resolved) => {
                    picked = Some((choice, resolved));
                    break;
                }
                Err(PlanError::UnsupportedPayload(_)) => continue,
                Err(PlanError::Failed(error)) => {
                    return Err(format!("{board} {}: {error}", choice.as_str()));
                }
            }
        }
        let Some((choice, resolved)) = picked else {
            continue;
        };
        if choice == PayloadChoice::Linux && !fits_kernel(&resolved) {
            continue;
        }
        selected.push((choice, resolved));
    }
    Ok(selected)
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn a_backup_alias_rejects_the_whole_output_set() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("vendor.rom");
        fs::write(&backup, b"original").unwrap();
        fs::hard_link(&backup, dir.path().join("z-full.rom")).unwrap();
        let files = [
            ("a-report.json".into(), vec![0]),
            ("z-full.rom".into(), vec![1]),
        ]
        .into();
        assert!(write_outputs(dir.path(), &files, true).is_err());
        assert!(!dir.path().join("a-report.json").exists());
        assert_eq!(fs::read(backup).unwrap(), b"original");
    }

    #[test]
    fn automatic_selection_falls_back_only_on_unsupported_payload() {
        let selected = select_payloads_for_board(
            "demo",
            false,
            None,
            |choice| match choice {
                PayloadChoice::UefiUi => Err(PlanError::unsupported_payload("no UI")),
                PayloadChoice::Uefi => Ok(true),
                PayloadChoice::Linux => Ok(false), // filtered by fits_kernel
                _ => Err(PlanError::unsupported_payload("unsupported")),
            },
            |&fits| fits,
        )
        .unwrap();
        assert_eq!(selected, vec![(PayloadChoice::Uefi, true)]);

        let err = select_payloads_for_board(
            "demo",
            false,
            None,
            |choice| match choice {
                PayloadChoice::UefiUi => Err(PlanError::failed("plan runner failed to compile")),
                PayloadChoice::Uefi => Ok(()),
                _ => Err(PlanError::unsupported_payload("unsupported")),
            },
            |_| true,
        )
        .unwrap_err();
        assert!(err.contains("demo uefi-ui"), "{err}");
        assert!(err.contains("plan runner failed to compile"), "{err}");
    }

    #[test]
    fn explicit_payload_skips_inventory_only_on_unsupported_and_always_fails_for_explicit_board() {
        // Explicit payload across inventory: unsupported is skipped, fatal fails.
        let skipped = select_payloads_for_board(
            "demo",
            false,
            Some(PayloadChoice::Coreboot),
            |_| Err(PlanError::unsupported_payload("no coreboot")),
            |_: &()| true,
        )
        .unwrap();
        assert!(skipped.is_empty());

        let fatal = select_payloads_for_board(
            "demo",
            false,
            Some(PayloadChoice::Coreboot),
            |_| Err(PlanError::failed("broken Cargo.lock")),
            |_: &()| true,
        )
        .unwrap_err();
        assert!(fatal.contains("demo coreboot"), "{fatal}");
        assert!(fatal.contains("broken Cargo.lock"), "{fatal}");

        // Explicit board + payload: unsupported is also an error.
        let unsupported = select_payloads_for_board(
            "demo",
            true,
            Some(PayloadChoice::Coreboot),
            |_| Err(PlanError::unsupported_payload("no coreboot")),
            |_: &()| true,
        )
        .unwrap_err();
        assert!(unsupported.contains("demo coreboot"), "{unsupported}");
        assert!(unsupported.contains("no coreboot"), "{unsupported}");
    }
}
