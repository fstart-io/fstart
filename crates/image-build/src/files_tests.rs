// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::os::unix::fs::symlink;

#[test]
fn only_plain_names_under_the_blob_directory_are_blobs() {
    let named = |path: &str| Blob::named(Path::new(path)).map(|b| b.map(|b| b.path().to_owned()));
    assert_eq!(
        named("/inputs/blobs/psp/a.bin"),
        Ok(Some("psp/a.bin".into()))
    );
    assert_eq!(named("/inputs/blobsx/a.bin"), Ok(None));
    assert_eq!(named("/boot/kernel"), Ok(None));
    assert_eq!(named("inputs/blobs/a.bin"), Ok(None));
    for escape in [
        "/inputs/blobs/../keys/key.pem",
        "/inputs/blobs/a/../../b",
        "/inputs/blobs",
    ] {
        assert!(named(escape).is_err(), "{escape}");
    }
}

#[test]
fn host_paths_are_not_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = dir.path().join("blobs");
    std::fs::create_dir(&blobs).unwrap();
    std::fs::write(blobs.join("a.bin"), b"blob").unwrap();
    let files = HostFiles {
        blobs: Some(blobs.clone()),
    };
    // A host path is the host's, wherever it is: it never reaches the
    // blob directory.
    assert!(files.read(Path::new("/inputs/blobs/a.bin")).is_err());
    assert!(!files.exists(Path::new("/inputs/blobs/a.bin")));
    let root = dir.path();
    assert_eq!(
        files.read_named(root, Path::new("/inputs/blobs/a.bin")),
        Ok(b"blob".to_vec())
    );
    assert_eq!(
        files.named_len(root, Path::new("/inputs/blobs/a.bin")),
        Ok(4)
    );
    // A relative path the plan names is the workspace's.
    std::fs::write(root.join("w.bin"), b"workspace").unwrap();
    assert_eq!(
        files.read_named(root, Path::new("w.bin")),
        Ok(b"workspace".to_vec())
    );
    assert_eq!(
        files.named_exists(root, Path::new("/inputs/blobs/b.bin")),
        Ok(false)
    );
}

#[test]
fn blobs_stay_in_the_blob_directory() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = dir.path().join("blobs");
    std::fs::create_dir_all(blobs.join("psp")).unwrap();
    std::fs::write(dir.path().join("key.pem"), b"secret").unwrap();
    std::fs::write(blobs.join("psp/a.bin"), b"blob").unwrap();
    // Links within the directory are its files; links out of it are not.
    symlink("psp/a.bin", blobs.join("inside.bin")).unwrap();
    symlink("../key.pem", blobs.join("key.bin")).unwrap();
    symlink(dir.path(), blobs.join("up")).unwrap();
    symlink("../missing", blobs.join("dangling.bin")).unwrap();
    let files = HostFiles {
        blobs: Some(blobs.clone()),
    };
    let root = Path::new("/");
    let read = |path: &str| files.read_named(root, Path::new(path));
    assert_eq!(read("/inputs/blobs/inside.bin"), Ok(b"blob".to_vec()));
    for escape in [
        "/inputs/blobs/key.bin",
        "/inputs/blobs/up/key.pem",
        "/inputs/blobs/../key.pem",
        "/inputs/blobs/dangling.bin",
        "/inputs/blobs/psp",
    ] {
        let error = read(escape).unwrap_err();
        assert!(!error.contains("secret"), "{error}");
        assert!(
            files.named_exists(root, Path::new(escape)).is_err(),
            "{escape}"
        );
    }
    // Without a blob directory, there are no blobs.
    assert!(
        HostFiles::default()
            .read_named(root, Path::new("/inputs/blobs/psp/a.bin"))
            .is_err()
    );
}

#[test]
fn a_memory_tree_has_its_blobs_in_the_blob_directory() {
    let files = MemoryFiles::default();
    files
        .write(Path::new("/inputs/blobs/a.bin"), b"blob")
        .unwrap();
    files.write(Path::new("/inputs/key.bin"), b"key").unwrap();
    let root = Path::new("/workspace");
    assert_eq!(
        files.read_named(root, Path::new("/inputs/blobs/a.bin")),
        Ok(b"blob".to_vec())
    );
    assert!(
        files
            .read_named(root, Path::new("/inputs/blobs/../key.bin"))
            .is_err()
    );
    assert_eq!(
        files.named_exists(root, Path::new("/inputs/blobs/b.bin")),
        Ok(false)
    );
}

#[test]
fn host_writes_replace_the_file_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.rom");
    let files = HostFiles::default();
    files.write(&path, b"old").unwrap();
    files.write(&path, b"new image").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new image");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
