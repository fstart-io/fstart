//! Assembly byte sources/sinks. The native tool uses disk; browser assembly
//! uses an isolated in-memory map and never opens paths on the host.
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
}

pub struct HostFiles;
impl Files for HostFiles {
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn create_dir_all(&self, path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|e| e.to_string())
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
