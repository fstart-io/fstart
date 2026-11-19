//! Where image assembly reads its inputs and writes its outputs. `fbuild`
//! uses the disk ([`HostFiles`]); a bundle assembled without a compiler,
//! in a browser among others, uses an isolated in-memory tree
//! ([`MemoryFiles`]) and never opens a host path.
//!
//! A plan names the vendor blobs it packs under [`BLOBS_DIR`], never by
//! where they are on a host: the plan, and the units compiled from it,
//! are the same for every blob directory with the same files. Only a path
//! the plan names there is a blob ([`Blob`]), read from the blob directory
//! and nowhere else; every other path is the host's, as is.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub trait Files {
    fn read(&self, path: &Path) -> Result<Vec<u8>, String>;
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String>;
    fn exists(&self, path: &Path) -> bool;
    fn create_dir_all(&self, path: &Path) -> Result<(), String>;
    /// The size of the file at `path`.
    fn len(&self, path: &Path) -> Result<u64, String> {
        self.read(path).map(|bytes| bytes.len() as u64)
    }
    /// The blob directory's file `blob`, if it has it. Blobs are only read.
    fn blob(&self, blob: Blob<'_>) -> Result<Option<Vec<u8>>, String>;
    /// The size of the blob directory's file `blob`, if it has it.
    fn blob_len(&self, blob: Blob<'_>) -> Result<Option<u64>, String> {
        self.blob(blob)
            .map(|bytes| bytes.map(|bytes| bytes.len() as u64))
    }

    /// The file a plan names `path`: a blob ([`Blob::named`]), or a host
    /// path relative to `root`.
    fn read_named(&self, root: &Path, path: &Path) -> Result<Vec<u8>, String> {
        match Blob::named(path)? {
            Some(blob) => self
                .blob(blob)?
                .ok_or_else(|| format!("{}: not in the blob directory", path.display())),
            None => self.read(&root.join(path)),
        }
    }
    /// The size of the file a plan names `path` ([`Files::read_named`]).
    fn named_len(&self, root: &Path, path: &Path) -> Result<u64, String> {
        match Blob::named(path)? {
            Some(blob) => self
                .blob_len(blob)?
                .ok_or_else(|| format!("{}: not in the blob directory", path.display())),
            None => self.len(&root.join(path)),
        }
    }
    /// Whether the file a plan names `path` exists ([`Files::read_named`]).
    /// A blob reference that leaves the blob directory is an error, not
    /// absent.
    fn named_exists(&self, root: &Path, path: &Path) -> Result<bool, String> {
        match Blob::named(path)? {
            Some(blob) => Ok(self.blob_len(blob)?.is_some()),
            None => Ok(self.exists(&root.join(path))),
        }
    }
}

/// The blob directory as a plan names it: a bundle's assembly writes the
/// blobs it extracts from the donor there, [`HostFiles`] reads them from
/// the build's blob directory.
pub const BLOBS_DIR: &str = "/inputs/blobs";

/// A file of the blob directory: a relative path of plain components, so
/// it stays in the directory lexically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blob<'a>(&'a Path);

impl<'a> Blob<'a> {
    /// The blob a plan names `path`, if it names one (a path under
    /// [`BLOBS_DIR`]). A name there that is not a plain relative path in
    /// it (`..`, nothing) is an error.
    pub fn named(path: &'a Path) -> Result<Option<Self>, String> {
        let Ok(file) = path.strip_prefix(BLOBS_DIR) else {
            return Ok(None);
        };
        let plain = file.components().next().is_some()
            && file.components().all(|c| matches!(c, Component::Normal(_)));
        if !plain {
            return Err(format!(
                "{}: not a file of the blob directory",
                path.display()
            ));
        }
        Ok(Some(Self(file)))
    }
    /// Its path in the blob directory.
    pub fn path(&self) -> &'a Path {
        self.0
    }
}

/// The host's filesystem, with the blobs in `blobs`.
#[derive(Debug, Default)]
pub struct HostFiles {
    /// The build's blob directory (`--blobs`, or the one `--donor`
    /// fills).
    pub blobs: Option<PathBuf>,
}

impl HostFiles {
    /// Where `blob` is on the host: in the blob directory with every
    /// symlink resolved, or None if it is not there.
    fn blob_path(&self, blob: Blob<'_>) -> Result<Option<PathBuf>, String> {
        let dir = self
            .blobs
            .as_ref()
            .ok_or("the build has no blob directory")?;
        let root = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = root.join(blob.path());
        let real = match std::fs::canonicalize(&path) {
            Ok(real) => real,
            // Absent, not a dangling link.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&path).is_err() =>
            {
                return Ok(None);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if !real.starts_with(&root) {
            return Err(format!(
                "{}: leaves the blob directory {}",
                path.display(),
                root.display()
            ));
        }
        if !real.is_file() {
            return Err(format!("{}: not a file", path.display()));
        }
        Ok(Some(real))
    }
}

impl Files for HostFiles {
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    }
    /// Written next to `path`, then renamed over it: a concurrent reader
    /// sees the old file or the new one, never a part.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let name = path
            .file_name()
            .ok_or_else(|| format!("{}: not a file", path.display()))?;
        let mut partial = name.to_owned();
        partial.push(format!(".{}.partial", std::process::id()));
        let partial = path.with_file_name(partial);
        std::fs::write(&partial, bytes)
            .and_then(|()| std::fs::rename(&partial, path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                format!("{}: {e}", path.display())
            })
    }
    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn create_dir_all(&self, path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))
    }
    fn len(&self, path: &Path) -> Result<u64, String> {
        std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| format!("{}: {e}", path.display()))
    }
    fn blob(&self, blob: Blob<'_>) -> Result<Option<Vec<u8>>, String> {
        let Some(path) = self.blob_path(blob)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(bytes))
    }
    fn blob_len(&self, blob: Blob<'_>) -> Result<Option<u64>, String> {
        self.blob_path(blob)?
            .map(|path| self.len(&path))
            .transpose()
    }
}

/// Lexical normalization only: no host filesystem access, symlinks or cwd.
pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// An in-memory tree of files by normalized path. Its blobs are its files
/// in [`BLOBS_DIR`].
#[derive(Default)]
pub struct MemoryFiles(pub RefCell<BTreeMap<PathBuf, Vec<u8>>>);

impl Files for MemoryFiles {
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        self.0
            .borrow()
            .get(&normalize(path))
            .cloned()
            .ok_or_else(|| format!("missing input {}", path.display()))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.0.borrow_mut().insert(normalize(path), bytes.to_vec());
        Ok(())
    }
    fn exists(&self, path: &Path) -> bool {
        self.0.borrow().contains_key(&normalize(path))
    }
    fn create_dir_all(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }
    fn blob(&self, blob: Blob<'_>) -> Result<Option<Vec<u8>>, String> {
        Ok(self
            .0
            .borrow()
            .get(&Path::new(BLOBS_DIR).join(blob.path()))
            .cloned())
    }
    fn blob_len(&self, blob: Blob<'_>) -> Result<Option<u64>, String> {
        Ok(self
            .0
            .borrow()
            .get(&Path::new(BLOBS_DIR).join(blob.path()))
            .map(|bytes| bytes.len() as u64))
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
