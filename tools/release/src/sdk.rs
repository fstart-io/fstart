use crate::Result;
use std::{
    fs::{self, File},
    io,
    path::Path,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub(super) fn archive(root: &Path, output: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    let output = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?
        .join(output.file_name().ok_or("missing archive filename")?);
    if output.starts_with(&root) {
        return Err("SDK archive must be outside its source tree".into());
    }
    let file = File::options().write(true).create_new(true).open(&output)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(9));
    add(&mut zip, &root, &root, options)?;
    zip.finish()?;
    Ok(())
}
fn add(
    zip: &mut ZipWriter<File>,
    root: &Path,
    directory: &Path,
    options: SimpleFileOptions,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("SDK files must not be symbolic links".into());
        }
        if kind.is_dir() {
            if entry.file_name() != "node" {
                add(zip, root, &entry.path(), options)?;
            }
        } else if kind.is_file() {
            let path = entry.path();
            zip.start_file_from_path(path.strip_prefix(root)?, options)?;
            io::copy(&mut File::open(path)?, zip)?;
        } else {
            return Err("unsupported SDK file type".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn compresses_sdk_and_omits_node_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sdk");
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::create_dir(root.join("node")).unwrap();
        fs::write(root.join(".nojekyll"), []).unwrap();
        fs::write(root.join("pkg/module.wasm"), vec![42; 65536]).unwrap();
        fs::write(root.join("node/test-only.js"), b"excluded").unwrap();
        let out = temp.path().join("sdk.zip");
        archive(&root, &out).unwrap();
        assert!(fs::metadata(&out).unwrap().len() < 1024);
        let mut zip = zip::ZipArchive::new(File::open(out).unwrap()).unwrap();
        assert_eq!(zip.len(), 2);
        assert!(zip.by_name(".nojekyll").is_ok());
        let mut wasm = zip.by_name("pkg/module.wasm").unwrap();
        assert_eq!(wasm.compression(), CompressionMethod::Deflated);
        let mut bytes = Vec::new();
        wasm.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![42; 65536]);
    }

    #[test]
    fn refuses_recursive_output() {
        let temp = tempfile::tempdir().unwrap();
        assert!(archive(temp.path(), &temp.path().join("self.zip")).is_err());
    }
}
