//! Where image assembly reads its inputs and writes its outputs. `fbuild`
//! uses the disk ([`HostFiles`]); a bundle assembled without a compiler,
//! in a browser among others, uses an isolated in-memory tree
//! ([`MemoryFiles`]) and never opens a host path.
use std::{
    cell::RefCell,
    collections::BTreeMap,
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
}

/// The host's filesystem.
pub struct HostFiles;

impl Files for HostFiles {
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
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

/// An in-memory tree of files by normalized path.
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
}
