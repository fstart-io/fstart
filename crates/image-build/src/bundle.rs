//! Versioned, DEFLATE-compressed assembly bundles: what CI compiled for one
//! board and payload, with the plan's image steps, so an image can be
//! assembled without a compiler. ZIP members are read into memory, never
//! extracted to host paths; `fbuild compose-bundle` and the browser
//! assembler (wasm32) both use this API.
//!
//! A bundle never carries vendor firmware: an AMD build carries the blob
//! tables, and its assembly extracts the blobs from the user's donor image
//! into [`BLOBS_DIR`], where its plan's image steps read them.
use crate::{
    StageBinary,
    build_plan::{BuildPlan, InputFile},
    files::{Files, MemoryFiles, normalize},
    image_steps::{ImageContext, execute},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

pub const VERSION: u32 = 3;
/// Where a bundle's assembly finds the blobs it extracts from the donor,
/// and the blob directory its AMD plan names.
pub use crate::files::BLOBS_DIR;
/// The workspace root of a bundle's files.
pub const WORKSPACE: &str = "/workspace";
/// Where a bundle's assembly writes the image.
pub const OUTPUT: &str = "/output";
// SBSA's full pflash image is 256 MiB, even though most bytes are erased.
pub const MAX_FILE: u64 = 256 << 20;
pub use crate::donor::{MAX_DONOR, MAX_DONOR_COPIES};
pub const MAX_BUNDLE: u64 = 512 << 20;

pub fn sha256(bytes: &[u8]) -> String {
    fstart_crypto::digest::hash_sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub member: String,
    pub size: u64,
    pub sha256: String,
}

/// The blob tables an AMD build's assembly extracts its PSP firmware with
/// from the user's donor image: one SoC table and the board's for each SoC
/// (each generation of an SP3 board).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackedDonor {
    pub tables: Vec<(crate::amd::blobs::SocBlobs, crate::amd::blobs::BoardBlobs)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub board: String,
    pub payload: String,
    pub revision: String,
    pub board_dir: PathBuf,
    /// The build's plan: its image steps, inputs and options.
    pub plan: BuildPlan,
    /// The plan's stage binaries, at their paths in the bundle.
    pub stages: Vec<StageBinary>,
    /// Kernel/runtime firmware inputs intentionally not redistributed here.
    pub inputs: Vec<InputFile>,
    pub packed_donor: Option<PackedDonor>,
    /// A payload CI built and shipped in the bundle (SeaBIOS for the coreboot
    /// payload). It fills its input slot when the user supplies none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundled_payload: Option<BundledPayload>,
    /// Logical assembly path -> archive member and content identity.
    pub files: BTreeMap<PathBuf, Member>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledPayload {
    /// Human-readable name and version, e.g. "SeaBIOS 1.17.0".
    pub name: String,
    /// The input it fills (`kernel` for the coreboot payload) and its file.
    pub input: String,
    pub path: PathBuf,
    /// Files handed to the payload with it (name, path in the bundle). Only
    /// used with the bundled payload, not with one the user supplies.
    #[serde(default)]
    pub files: Vec<(String, PathBuf)>,
}

/// What an assembly takes besides the bundle. The donor and the signing
/// key are separate from `inputs`, so neither can be published in an
/// output bundle by accident.
#[derive(Debug, Default)]
pub struct Supplied<'a> {
    /// The vendor image an AMD build takes its PSP firmware from (a raw
    /// ROM, or a vendor update ZIP holding one).
    pub donor: Option<&'a [u8]>,
    /// Optional complete Intel descriptor-mode SPI image; only its BIOS region is replaced.
    pub intel_flash: Option<&'a [u8]>,
    /// Named kernel, firmware or FIT bytes, as the plan's inputs declare.
    pub inputs: BTreeMap<String, Vec<u8>>,
    pub signing_key: Option<&'a [u8]>,
    /// What the image's settings store starts with, checked against the
    /// plan's options.
    pub settings: crate::options::Settings,
}

pub struct Bundle {
    pub manifest: Manifest,
    pub files: MemoryFiles,
}

impl Bundle {
    pub fn encode(&mut self) -> Result<Vec<u8>, String> {
        self.manifest.check()?;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(9));
        self.manifest.files.clear();
        for (path, data) in self.files.0.borrow().iter() {
            if data.len() as u64 > MAX_FILE {
                return Err("bundle input too large".into());
            }
            let relative = path
                .strip_prefix("/")
                .map_err(|_| "bundle paths must be absolute")?;
            if normalize(path) != *path {
                return Err("bundle path is not normalized".into());
            }
            let member = format!("files/{}", relative.to_string_lossy());
            zip.start_file(&member, options)
                .map_err(|e| e.to_string())?;
            zip.write_all(data).map_err(|e| e.to_string())?;
            self.manifest.files.insert(
                path.clone(),
                Member {
                    member,
                    size: data.len() as u64,
                    sha256: sha256(data),
                },
            );
        }
        let manifest = serde_json::to_vec_pretty(&self.manifest).map_err(|e| e.to_string())?;
        zip.start_file("manifest.json", options)
            .map_err(|e| e.to_string())?;
        zip.write_all(&manifest).map_err(|e| e.to_string())?;
        Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_BUNDLE {
            return Err("bundle too large".into());
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        if zip.len() > 1024 {
            return Err("too many bundle members".into());
        }
        if zip.has_duplicate_names() {
            return Err("unexpected or duplicate bundle members".into());
        }
        let manifest: Manifest =
            serde_json::from_slice(&read_member(&mut zip, "manifest.json", 4 << 20)?)
                .map_err(|e| format!("bundle manifest: {e}"))?;
        manifest.check()?;
        if zip.len() != manifest.files.len() + 1 {
            return Err("unexpected or duplicate bundle members".into());
        }
        let files = MemoryFiles::default();
        let mut members = BTreeSet::new();
        let mut total = 0u64;
        for (path, member) in &manifest.files {
            total = total
                .checked_add(member.size)
                .filter(|&n| n <= MAX_BUNDLE)
                .ok_or("expanded bundle too large")?;
            if !member.member.starts_with("files/")
                || !members.insert(&member.member)
                || member.size > MAX_FILE
                || normalize(path) != *path
            {
                return Err("invalid bundle member".into());
            }
            let data = read_member(&mut zip, &member.member, member.size)?;
            if data.len() as u64 != member.size || sha256(&data) != member.sha256 {
                return Err(format!("bundle checksum mismatch: {}", path.display()));
            }
            files.write(path, &data)?;
        }
        Ok(Self { manifest, files })
    }

    /// The donor's flash image in `bytes`: a raw ROM, or the image of a
    /// vendor update ZIP with firmware of the bundle's blob tables
    /// ([`crate::donor::image`]).
    pub fn donor(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let tables = self.manifest.packed_donor.as_ref();
        let usable = |image: &[u8]| {
            tables.is_none_or(|tables| crate::amd::blobs::has_firmware(&tables.tables, image))
        };
        Ok(crate::donor::image(bytes, &usable)?.bytes.into_owned())
    }

    fn intel_layout(&self) -> Result<fstart_core::IntelIfdFlashLayout, String> {
        match self
            .manifest
            .plan
            .ffs_assembly()?
            .flash
            .as_ref()
            .map(crate::plan::FlashTransport::decode)
            .transpose()?
        {
            Some(fstart_core::FlashLayout::IntelIfd(ifd)) => Ok(ifd),
            _ => Err("this build takes no Intel descriptor-mode flash image".into()),
        }
    }

    /// Read-only preflight for a raw vendor image. Return the checked region
    /// ranges so clients can show/extract them without decoding Intel registers.
    pub fn inspect_intel_flash(&self, bytes: &[u8]) -> Result<crate::plan::IfdTransport, String> {
        let layout = self.intel_layout()?;
        crate::intel_flash::validate(&layout, bytes)?;
        Ok(layout.into())
    }

    /// Run the plan's image steps over the bundle's files and `supplied`,
    /// in memory. Returns the files the steps wrote, by name, and an
    /// `assembly-report.json` of what went in and came out.
    pub fn assemble(&self, supplied: &Supplied<'_>) -> Result<BTreeMap<String, Vec<u8>>, String> {
        let manifest = &self.manifest;
        manifest.check()?;
        let files = MemoryFiles(self.files.0.clone());
        let mut donor_sha256 = None;
        match (supplied.donor, &manifest.packed_donor) {
            (None, Some(_)) => return Err("this build requires a local vendor BIOS image".into()),
            (Some(donor), Some(tables)) => {
                let donor = self.donor(donor)?;
                donor_sha256 = Some(sha256(&donor));
                let donated = crate::amd::blobs::extract_donor(
                    &tables.tables,
                    &manifest.board,
                    &donor,
                    false,
                )?;
                for file in donated.found {
                    files.write(&Path::new(BLOBS_DIR).join(file.file), &file.bytes)?;
                }
            }
            (Some(_), None) => return Err("this build takes no donor image".into()),
            (None, None) => {}
        }
        let mut bound = BTreeMap::new();
        for (name, bytes) in &supplied.inputs {
            let input = (manifest.inputs.iter())
                .find(|i| i.name == *name)
                .ok_or_else(|| format!("unknown input {name}"))?;
            if bytes.is_empty() || bytes.len() as u64 > input.capacity {
                return Err(format!("{name} is empty or exceeds capacity"));
            }
            let path = Path::new("/inputs").join(name);
            files.write(&path, bytes)?;
            // A step that reads the input where its default lies (a board
            // DTB the bundle does not ship) finds the supplied bytes there.
            if let Some(default) = &input.default {
                files.write(Path::new(default), bytes)?;
            }
            bound.insert(name.clone(), path);
        }
        let bundled = (manifest.bundled_payload.as_ref())
            .filter(|payload| !supplied.inputs.contains_key(&payload.input));
        if let Some(payload) = bundled {
            bound.insert(payload.input.clone(), payload.path.clone());
        }
        for input in &manifest.inputs {
            if !bound.contains_key(&input.name) {
                match &input.default {
                    // A default the bundle carries.
                    Some(path) if files.exists(Path::new(path)) => {
                        bound.insert(input.name.clone(), path.into());
                    }
                    _ => return Err(format!("missing input {}", input.name)),
                }
            }
        }
        if manifest.plan.images.is_empty() {
            return Err("this build provides binaries only, not an image assembly contract".into());
        }
        let key_path = Path::new("/inputs/signing-key");
        if let Some(key) = supplied.signing_key {
            files.write(key_path, key)?;
        }
        let payload_files: Vec<(String, String)> = bundled
            .map(|payload| {
                (payload.files.iter())
                    .map(|(name, path)| (name.clone(), path.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let output_dir = Path::new(OUTPUT);
        execute(
            &manifest.plan,
            &ImageContext {
                files: &files,
                workspace_root: Path::new(WORKSPACE),
                board_dir: &manifest.board_dir,
                board_name: &manifest.board,
                stages: &manifest.stages,
                output_dir,
                inputs: &bound,
                payload_files: &payload_files,
                signing_key: supplied.signing_key.map(|_| key_path),
                settings: &supplied.settings,
            },
        )?;
        let mut outputs: BTreeMap<String, Vec<u8>> = (files.0.into_inner().into_iter())
            .filter_map(|(path, bytes)| {
                path.strip_prefix(output_dir)
                    .ok()
                    .map(|relative| (relative.to_string_lossy().into_owned(), bytes))
            })
            .collect();
        let mut intel_flash = None;
        if let Some(vendor) = supplied.intel_flash {
            let layout = self.intel_layout()?;
            let name = format!("{}-{}m.pflash", manifest.board, layout.size() >> 20);
            let partial = outputs.get(&name).ok_or("no assembled Intel flash image")?;
            let (full, regions) = crate::intel_flash::compose(&layout, vendor, partial)?;
            outputs.insert(format!("{}-full.rom", manifest.board), full);
            intel_flash = Some(IntelImport {
                sha256: sha256(vendor),
                preserved_regions: regions,
            });
        }
        #[derive(Serialize)]
        struct IntelImport {
            sha256: String,
            preserved_regions: Vec<crate::intel_flash::ImportedRegion>,
        }
        #[derive(Serialize)]
        struct Identity {
            size: usize,
            sha256: String,
        }
        #[derive(Serialize)]
        struct Report<'a> {
            format_version: u32,
            board: &'a str,
            payload: &'a str,
            revision: &'a str,
            donor_sha256: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            intel_flash: Option<IntelImport>,
            input_sha256: BTreeMap<&'a str, String>,
            settings: &'a [(String, String)],
            #[serde(skip_serializing_if = "Option::is_none")]
            secure_boot: Option<String>,
            outputs: BTreeMap<&'a str, Identity>,
        }
        let report = Report {
            format_version: VERSION,
            board: &manifest.board,
            payload: &manifest.payload,
            revision: &manifest.revision,
            donor_sha256,
            intel_flash,
            input_sha256: (supplied.inputs.iter())
                .map(|(name, bytes)| (name.as_str(), sha256(bytes)))
                .collect(),
            settings: &supplied.settings.options,
            secure_boot: (supplied.settings.secure_boot.as_ref()).map(|sb| sb.describe()),
            outputs: (outputs.iter())
                .map(|(name, bytes)| {
                    let identity = Identity {
                        size: bytes.len(),
                        sha256: sha256(bytes),
                    };
                    (name.as_str(), identity)
                })
                .collect(),
        };
        let report = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        outputs.insert("assembly-report.json".into(), report);
        Ok(outputs)
    }
}

impl Manifest {
    fn check(&self) -> Result<(), String> {
        if self.format_version != VERSION {
            return Err(format!("unsupported bundle format {}", self.format_version));
        }
        if self.board.is_empty()
            || !self
                .board
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err("invalid bundle board identity".into());
        }
        if self.stages.is_empty() {
            return Err("bundle has no stages".into());
        }
        Ok(())
    }
}

/// The ZIP member `name`, at most `max` bytes expanded.
fn read_member(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    let index = (zip.index_for_name(name)).ok_or_else(|| format!("no ZIP member {name}"))?;
    read_entry(zip, index, max)
}

/// The ZIP member at `index`, at most `max` bytes expanded.
pub(crate) fn read_entry(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    index: usize,
    max: u64,
) -> Result<Vec<u8>, String> {
    let file = zip.by_index(index).map_err(|e| e.to_string())?;
    if file.size() > max {
        let name = file.name().map_err(|e| e.to_string())?;
        return Err(format!("ZIP member {name} is too large"));
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("expanded ZIP member too large".into());
    }
    Ok(bytes)
}
