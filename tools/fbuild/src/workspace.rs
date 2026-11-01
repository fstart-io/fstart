//! Checkout-wide orchestration ownership, reentrant on one thread. Different
//! threads/processes still serialize; guards cannot cross threads. Editor
//! checks use the same lock, and launchers release it before interactive waits.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

thread_local! {
    static OWNERS: RefCell<BTreeMap<PathBuf, Weak<File>>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Debug, Clone)]
pub(crate) struct Guard {
    _file: Rc<File>,
}
impl Guard {
    pub fn acquire(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root.join("target")).map_err(|e| e.to_string())?;
        let path = root
            .join("target")
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("fstart-orchestration.lock");
        OWNERS.with(|owners| {
            let mut owners = owners.borrow_mut();
            if let Some(file) = owners.get(&path).and_then(Weak::upgrade) {
                return Ok(Self { _file: file });
            }
            let file = File::options()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            if file.try_lock().is_err() {
                eprintln!("[fstart] waiting for checkout orchestration lock...");
                file.lock()
                    .map_err(|e| format!("lock fbuild workspace: {e}"))?;
            }
            let file = Rc::new(file);
            owners.retain(|_, owner| owner.strong_count() != 0);
            owners.insert(path, Rc::downgrade(&file));
            Ok(Self { _file: file })
        })
    }
}

/// Publish complete bytes atomically and leave identical files untouched, so
/// editors and Cargo never observe truncation or unnecessary timestamp churn.
pub(crate) fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), String> {
    let bytes = bytes.as_ref();
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
        && fs::read(path).ok().as_deref() == Some(bytes)
    {
        return Ok(());
    }
    let parent = path.parent().ok_or("generated file has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.persist(path)
        .map_err(|e| format!("publish {}: {e}", path.display()))?;
    Ok(())
}

/// Change only Cargo's workspace membership, independent of TOML formatting.
pub(crate) fn manifest(original: &str, members: Vec<String>) -> Result<String, String> {
    let mut manifest: toml::Table = toml::from_str(original).map_err(|e| e.to_string())?;
    let workspace = manifest
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .ok_or("missing workspace table")?;
    workspace.insert(
        "members".into(),
        toml::Value::Array(members.into_iter().map(toml::Value::String).collect()),
    );
    workspace.remove("exclude");
    toml::to_string(&manifest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_does_not_depend_on_line_markers() {
        let original = "[workspace]\nmembers=[\"old\"] # compact\nexclude=[\"boards\"]\nresolver='3'\n[profile.release]\nlto=true\n";
        let result: toml::Table =
            toml::from_str(&manifest(original, vec!["boards/vendor/board".into()]).unwrap())
                .unwrap();
        assert_eq!(
            result["workspace"]["members"][0].as_str(),
            Some("boards/vendor/board")
        );
        assert!(result["workspace"].get("exclude").is_none());
        assert_eq!(result["profile"]["release"]["lto"].as_bool(), Some(true));
        assert!(manifest("[package]\nname='not-a-workspace'", vec![]).is_err());
    }
    #[test]
    fn nested_guards_do_not_deadlock_or_release_outer_ownership() {
        let root = tempfile::tempdir().unwrap();
        let outer = Guard::acquire(root.path()).unwrap();
        let inner = Guard::acquire(root.path()).unwrap();
        let competing = File::options()
            .read(true)
            .write(true)
            .open(root.path().join("target/fstart-orchestration.lock"))
            .unwrap();
        assert!(competing.try_lock().is_err());
        drop(inner);
        assert!(competing.try_lock().is_err());
        drop(outer);
        assert!(competing.try_lock().is_ok());
    }
    #[test]
    fn generated_files_never_reuse_a_symlink_to_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.lock");
        let generated = dir.path().join("Cargo.lock");
        fs::write(&source, "same").unwrap();
        std::os::unix::fs::symlink(&source, &generated).unwrap();
        write(&generated, "same").unwrap();
        assert!(!generated.is_symlink());
        write(&generated, "changed").unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "same");
    }

    #[test]
    fn identical_publications_keep_the_existing_file() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        write(&path, "first").unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        write(&path, "first").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        write(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert_ne!(fs::metadata(&path).unwrap().ino(), inode);
    }
}
