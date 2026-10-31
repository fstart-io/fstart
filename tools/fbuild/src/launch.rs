//! Private launch media has independent ownership. The next launch reaps
//! unlocked sessions left by killed fbuild processes; active sessions survive.
use std::{
    fs::{self, File},
    path::Path,
};

pub(crate) struct Session {
    // Drop the directory before closing its ownership lock on normal exit.
    directory: tempfile::TempDir,
    _owner: File,
}
impl Session {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn prune(parent: &Path) -> Result<(), String> {
    for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_name().to_string_lossy().starts_with("session-")
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let owner = entry.path().join("owner.lock");
        // No marker means an unknown/older directory, not proof of ownership.
        if !fs::symlink_metadata(&owner).is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        let file = match File::options().read(true).write(true).open(&owner) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if file.try_lock().is_err() {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(entry.path())
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.to_string());
        }
    }
    Ok(())
}

pub(crate) fn snapshot(
    root: &Path,
    guard: crate::workspace::Guard,
    sources: &[(&str, &Path)],
) -> Result<Session, String> {
    let parent = root.join("target/fstart-runs");
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    prune(&parent)?;
    let directory = tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let owner = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.path().join("owner.lock"))
        .map_err(|e| e.to_string())?;
    owner.lock().map_err(|e| e.to_string())?;
    for (name, source) in sources {
        if Path::new(name).components().count() != 1 {
            return Err("launch artifact must have a filename".into());
        }
        fs::copy(source, directory.path().join(name))
            .map_err(|e| format!("snapshot {}: {e}", source.display()))?;
    }
    drop(guard);
    Ok(Session {
        directory,
        _owner: owner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_long_lived_launcher_does_not_own_the_checkout_lock_or_mutable_outputs() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage.bin");
        fs::write(&source, b"published bytes").unwrap();
        let guard = crate::workspace::Guard::acquire(root.path()).unwrap();
        let session = snapshot(root.path(), guard, &[("firmware.bin", &source)]).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let checkout = root.path().to_owned();
        let worker = std::thread::spawn(move || {
            let _next_build = crate::workspace::Guard::acquire(&checkout).unwrap();
            fs::write(checkout.join("stage.bin"), b"next build bytes").unwrap();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("launcher retained the checkout lock");
        worker.join().unwrap();
        assert_eq!(
            fs::read(session.path().join("firmware.bin")).unwrap(),
            b"published bytes"
        );
    }
    #[test]
    fn the_next_launch_reaps_dead_owners_but_preserves_live_and_unknown_sessions() {
        if let Some(root) = std::env::var_os("ABANDON_LAUNCH_SESSION") {
            let root = Path::new(&root);
            let session =
                snapshot(root, crate::workspace::Guard::acquire(root).unwrap(), &[]).unwrap();
            fs::write(session.path().join("media.pflash"), [0; 4096]).unwrap();
            fs::write(
                root.join("abandoned-session"),
                session.path().to_str().unwrap(),
            )
            .unwrap();
            // Like SIGKILL: close kernel handles without running TempDir::drop.
            std::process::exit(0);
        }
        let root = tempfile::tempdir().unwrap();
        let first = snapshot(
            root.path(),
            crate::workspace::Guard::acquire(root.path()).unwrap(),
            &[],
        )
        .unwrap();
        let parent = root.path().join("target/fstart-runs");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "launch::tests::the_next_launch_reaps_dead_owners_but_preserves_live_and_unknown_sessions", "--nocapture"])
            .env("ABANDON_LAUNCH_SESSION", root.path()).output().unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let stale = std::path::PathBuf::from(
            fs::read_to_string(root.path().join("abandoned-session")).unwrap(),
        );
        assert!(stale.join("media.pflash").exists());
        let unknown = parent.join("session-unknown");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("user-file"), "keep").unwrap();
        let _second = snapshot(
            root.path(),
            crate::workspace::Guard::acquire(root.path()).unwrap(),
            &[],
        )
        .unwrap();
        assert!(!stale.exists());
        assert!(first.path().exists());
        assert!(unknown.join("user-file").exists());
    }
}
