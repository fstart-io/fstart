//! Explicit bounded compiler-file snapshots; no family policy or trust inference.
//! The build owner must exclude filesystem mutation through compiler execution.
use fstart_image_build::build_plan::FileBinding;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Input {
    pub environment: String,
    source: PathBuf,
    data: Vec<u8>,
}
#[derive(serde::Serialize)]
pub struct Record {
    environment: String,
    source: PathBuf,
    snapshot: PathBuf,
    bytes: usize,
    sha256: String,
}
fn read(path: &Path, capacity: u64) -> Result<Vec<u8>, String> {
    let limit = capacity
        .checked_add(1)
        .filter(|&size| usize::try_from(size).is_ok())
        .ok_or("compiler input capacity overflow")?;
    let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > capacity {
        return Err(format!(
            "{}: compiler input is not a nonempty bounded regular file",
            path.display()
        ));
    }
    let mut data = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.is_empty() || data.len() as u64 > capacity {
        return Err(format!(
            "{}: compiler input changed outside its size bound",
            path.display()
        ));
    }
    Ok(data)
}
pub fn collect(root: &Path, bindings: &[FileBinding]) -> Result<Vec<Input>, String> {
    bindings
        .iter()
        .map(|binding| {
            let source = root
                .join(&binding.path)
                .canonicalize()
                .map_err(|e| format!("{}: {e}", binding.path))?;
            let data = read(&source, binding.capacity)?;
            Ok(Input {
                environment: binding.environment.clone(),
                source,
                data,
            })
        })
        .collect()
}
impl Input {
    /// Host image operations may consume the same bounded immutable bytes without
    /// putting image-only namespaces into a firmware compiler environment.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
    /// Bind origin, namespace and actual bytes, not just a filename or mtime.
    pub fn hash(&self, hash: &mut Sha256) -> Result<(), String> {
        let source = self.source.to_str().ok_or("non-UTF8 compiler input path")?;
        for bytes in [
            b"compiler-file".as_slice(),
            self.environment.as_bytes(),
            source.as_bytes(),
            &self.data,
        ] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        Ok(())
    }
    pub fn publish(&self, directory: &Path) -> Result<(PathBuf, Record), String> {
        let files = directory.join("compiler-files");
        fs::create_dir_all(&files).map_err(|e| e.to_string())?;
        let snapshot = files.join(format!("{}.bin", self.environment));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&snapshot)
        {
            Ok(mut file) => {
                file.write_all(&self.data).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                let mut permissions = file.metadata().map_err(|e| e.to_string())?.permissions();
                permissions.set_readonly(true);
                file.set_permissions(permissions)
                    .map_err(|e| e.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Never overwrite, follow a cached symlink or accept corrupted
                // bytes. Interrupted/concurrent publication can fail closed.
                let metadata = fs::symlink_metadata(&snapshot).map_err(|e| e.to_string())?;
                if !metadata.is_file()
                    || !metadata.permissions().readonly()
                    || read(&snapshot, self.data.len() as u64)? != self.data
                {
                    return Err(format!("corrupt compiler snapshot {}", snapshot.display()));
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        let record = Record {
            environment: self.environment.clone(),
            source: self.source.clone(),
            snapshot: snapshot.clone(),
            bytes: self.data.len(),
            sha256: format!("{:x}", Sha256::digest(&self.data)),
        };
        Ok((snapshot, record))
    }
}
