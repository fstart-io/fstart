//! Versioned, DEFLATE-compressed assembly bundles. ZIP members are read into
//! memory, never extracted to host paths. Both the CLI and WASM use this API.
use crate::{
    StageBinary,
    build_plan::{Assembly, InputFile},
    files::{Files, MemoryFiles, normalize},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;

pub const VERSION: u32 = 2;
pub const DONOR_PATH: &str = "/inputs/donor.rom";
// SBSA's full pflash image is 256 MiB, even though most bytes are erased.
pub const MAX_FILE: u64 = 256 << 20;
pub const MAX_DONOR: u64 = 128 << 20;
pub const MAX_BUNDLE: u64 = 512 << 20;

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub member: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackedDonor {
    pub soc: crate::amd::blobs::SocBlobs,
    pub board: crate::amd::blobs::BoardBlobs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub board: String,
    pub payload: String,
    pub revision: String,
    pub board_dir: PathBuf,
    pub assembly: Assembly,
    pub stages: Vec<StageBinary>,
    /// Kernel/runtime firmware inputs intentionally not redistributed here.
    pub inputs: Vec<InputFile>,
    pub packed_donor: Option<PackedDonor>,
    /// Logical assembly path -> archive member and content identity.
    pub files: BTreeMap<PathBuf, Member>,
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

    /// ZIP and raw ROM are accepted. Ambiguous ZIPs must be unpacked by the user.
    pub fn donor(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        if bytes.len() as u64 > MAX_DONOR {
            return Err("donor too large".into());
        }
        if !bytes.starts_with(b"PK\x03\x04") {
            return Ok(bytes.to_vec());
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let names: Vec<String> = (0..zip.len())
            .filter_map(|index| {
                let file = zip.by_index(index).ok()?;
                let name = file.name().to_owned();
                let ext = Path::new(&name).extension()?.to_str()?.to_ascii_lowercase();
                ((1 << 20..=MAX_DONOR).contains(&file.size())
                    && matches!(ext.as_str(), "bin" | "rom" | "fd"))
                .then_some(name)
            })
            .collect();
        let [name] = names.as_slice() else {
            return Err(
                "donor ZIP must contain exactly one ROM/bin/fd image; unpack and select the ROM"
                    .into(),
            );
        };
        read_member(&mut zip, name, MAX_DONOR)
    }

    /// `inputs` are named kernel/firmware/fit bytes; donor and signing key are
    /// separate so neither can accidentally be published in an output bundle.
    pub fn assemble(
        &self,
        donor: Option<&[u8]>,
        inputs: &BTreeMap<String, Vec<u8>>,
        signing_key: Option<&[u8]>,
    ) -> Result<BTreeMap<String, Vec<u8>>, String> {
        self.manifest.check()?;
        let files = MemoryFiles(self.files.0.clone());
        if matches!(self.manifest.assembly, Assembly::Amd(_)) && donor.is_none() {
            return Err("this build requires a local vendor BIOS image".into());
        }
        let mut donor_sha256 = None;
        if let Some(donor) = donor {
            let donor = self.donor(donor)?;
            donor_sha256 = Some(sha256(&donor));
            if let Some(tables) = &self.manifest.packed_donor {
                let (found, missing) = crate::amd::blobs::extract(
                    &tables.soc,
                    &tables.board,
                    &self.manifest.board,
                    &donor,
                )?;
                if !missing.is_empty() {
                    return Err(format!("donor lacks {}", missing.join(", ")));
                }
                for file in found {
                    files.write(&Path::new("/inputs/blobs").join(file.file), &file.bytes)?;
                }
            } else {
                if let Assembly::Amd(image) = &self.manifest.assembly {
                    image.check_vendor(&donor)?;
                }
                files.write(Path::new(DONOR_PATH), &donor)?;
            }
        }
        for (name, bytes) in inputs {
            let input = self
                .manifest
                .inputs
                .iter()
                .find(|i| i.name == *name)
                .ok_or_else(|| format!("unknown input {name}"))?;
            if bytes.is_empty() || bytes.len() as u64 > input.capacity {
                return Err(format!("{name} is empty or exceeds capacity"));
            }
            files.write(&Path::new("/inputs").join(name), bytes)?;
            if let Some(path) = &input.default
                && path.starts_with("/workspace/")
            {
                files.write(Path::new(path), bytes)?;
            }
        }
        for input in &self.manifest.inputs {
            if !inputs.contains_key(&input.name) {
                return Err(format!("missing input {}", input.name));
            }
        }
        let key_path = signing_key
            .map(|key| {
                let path = Path::new("/inputs/signing-key");
                files.write(path, key).map(|()| path)
            })
            .transpose()?;
        let manifest = &self.manifest;
        let path_of = |name: &str| inputs.contains_key(name).then(|| format!("/inputs/{name}"));
        let kernel = path_of("kernel");
        let firmware = path_of("firmware");
        let fit = path_of("fit");
        let ffs = if manifest.assembly.ffs().is_ok() {
            let config = manifest.assembly.config(&manifest.board)?;
            Some(crate::assemble::assemble_with_files(
                &files,
                Path::new("/workspace"),
                &manifest.board_dir,
                &config,
                &manifest.stages,
                Some(Path::new("/output")),
                kernel.as_deref(),
                firmware.as_deref(),
                fit.as_deref(),
                &[],
                Some(manifest.assembly.bootstrap()),
                key_path,
            )?)
        } else {
            None
        };
        let reset = |unit: &str| -> Result<Vec<u8>, String> {
            let stage = manifest
                .stages
                .iter()
                .find(|s| s.name == unit)
                .ok_or("missing reset unit")?;
            files.read(&stage.run_path)
        };
        match &manifest.assembly {
            Assembly::Amd(image) => {
                let sources = image
                    .bindings()?
                    .iter()
                    .map(|b| files.read(&Path::new("/workspace").join(&b.path)))
                    .collect::<Result<Vec<_>, _>>()?;
                let refs: Vec<_> = sources.iter().map(Vec::as_slice).collect();
                let ffs = ffs.as_ref().map(|path| files.read(path)).transpose()?;
                let result = image.assemble(&refs, &reset(&image.reset_unit)?, ffs.as_deref())?;
                if !result.is_whole_flash() {
                    return Err("assembly produced only a PSP fragment".into());
                }
                files.write(
                    &Path::new("/output").join(format!("{}.rom", manifest.board)),
                    &result.bytes,
                )?;
                if let Some(bank) = result.boot_bank() {
                    files.write(
                        &Path::new("/output").join(format!("{}.bank.rom", manifest.board)),
                        bank,
                    )?;
                }
            }
            Assembly::Unassembled { .. } => {
                return Err(
                    "this build provides binaries only, not an image assembly contract".into(),
                );
            }
            Assembly::Ffs(_) => {}
        }
        let mut outputs: BTreeMap<String, Vec<u8>> = files
            .0
            .into_inner()
            .into_iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix("/output")
                    .ok()
                    .map(|relative| (relative.to_string_lossy().into_owned(), bytes))
            })
            .collect();
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
            input_sha256: BTreeMap<&'a str, String>,
            outputs: BTreeMap<&'a str, Identity>,
        }
        let report = Report {
            format_version: VERSION,
            board: &manifest.board,
            payload: &manifest.payload,
            revision: &manifest.revision,
            donor_sha256,
            input_sha256: inputs
                .iter()
                .map(|(name, bytes)| (name.as_str(), sha256(bytes)))
                .collect(),
            outputs: outputs
                .iter()
                .map(|(name, bytes)| {
                    (
                        name.as_str(),
                        Identity {
                            size: bytes.len(),
                            sha256: sha256(bytes),
                        },
                    )
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

fn read_member(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    let file = zip.by_name(name).map_err(|e| e.to_string())?;
    if file.size() > max {
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
