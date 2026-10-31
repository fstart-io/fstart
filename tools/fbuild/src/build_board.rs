use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "layout_linker_tests.rs"]
mod layout_linker_tests;

pub struct BuildResult {
    pub stages: Vec<fstart_image_build::StageBinary>,
}

impl BuildResult {
    pub fn primary_binary(&self) -> &fstart_image_build::StageBinary {
        &self.stages[0]
    }
}

pub fn prepare_selected_board_workspace(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
) -> Result<PathBuf, String> {
    let selected = workspace_root
        .join("target")
        .join("fstart-workspaces")
        .join(&board_manifest.board);
    let board_link = selected.join("boards").join(&board_manifest.rel_dir);
    let board_link_parent = board_link
        .parent()
        .ok_or_else(|| "board workspace link has no parent".to_string())?;
    fs::create_dir_all(board_link_parent)
        .map_err(|e| format!("failed to create selected board workspace: {e}"))?;
    fs::create_dir_all(selected.join("tools"))
        .map_err(|e| format!("failed to create selected tools dir: {e}"))?;

    replace_with_symlink(workspace_root.join("crates"), selected.join("crates"))?;
    replace_with_symlink(
        workspace_root.join("tools").join("fbuild"),
        selected.join("tools").join("fbuild"),
    )?;
    replace_with_file_copy(
        workspace_root.join("Cargo.lock"),
        selected.join("Cargo.lock"),
    )?;
    replace_with_symlink(board_manifest.dir.clone(), board_link)?;

    let rel_dir = board_manifest.rel_dir.to_str().ok_or_else(|| {
        format!(
            "board directory is not valid UTF-8: {}",
            board_manifest.rel_dir.display()
        )
    })?;
    let root_manifest = fs::read_to_string(workspace_root.join("Cargo.toml"))
        .map_err(|e| format!("failed to read root Cargo.toml: {e}"))?;
    let manifest = selected_workspace_manifest(&root_manifest, rel_dir)?;
    crate::workspace::write(&selected.join("Cargo.toml"), manifest)
        .map_err(|e| format!("failed to write selected board workspace manifest: {e}"))?;
    Ok(selected)
}

/// Disposable all-board lock prototype. Never replaces the source workspace.
pub(crate) fn prepare_inventory_workspace(
    root: &Path,
    boards: &[crate::board_manifest::BoardManifest],
) -> Result<PathBuf, String> {
    let directory = root.join("target/fstart-lock-prototype");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for name in ["crates", "tools", "boards"] {
        replace_with_symlink(root.join(name), directory.join(name))?;
    }
    replace_with_file_copy(root.join("Cargo.lock"), directory.join("Cargo.lock"))?;
    let original = fs::read_to_string(root.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let parsed: toml::Table = toml::from_str(&original).map_err(|e| e.to_string())?;
    let mut members = parsed
        .get("workspace")
        .and_then(|value| value.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or("missing workspace members")?
        .iter()
        .map(|member| {
            member
                .as_str()
                .map(String::from)
                .ok_or("invalid workspace member")
        })
        .collect::<Result<Vec<_>, _>>()?;
    members.extend(
        boards
            .iter()
            .map(|board| format!("boards/{}", board.rel_dir.display())),
    );
    let manifest = crate::workspace::manifest(&original, members)?;
    crate::workspace::write(&directory.join("Cargo.toml"), manifest)?;
    Ok(directory)
}

fn replace_with_symlink(target: PathBuf, link: PathBuf) -> Result<(), String> {
    if fs::read_link(&link).ok().as_ref() == Some(&target) {
        return Ok(());
    }
    let parent = link.parent().ok_or("workspace link has no parent")?;
    let temporary = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
    let staging = temporary.path().join("link");
    std::os::unix::fs::symlink(&target, &staging).map_err(|e| e.to_string())?;
    fs::rename(staging, &link)
        .map_err(|e| format!("publish workspace link {}: {e}", link.display()))
}

fn replace_with_file_copy(source: PathBuf, destination: PathBuf) -> Result<(), String> {
    crate::workspace::write(
        &destination,
        fs::read(&source).map_err(|e| format!("{}: {e}", source.display()))?,
    )
}

fn selected_workspace_manifest(root_manifest: &str, board_rel_dir: &str) -> Result<String, String> {
    crate::workspace::manifest(root_manifest, vec![format!("boards/{board_rel_dir}")])
}

/// The selected workspace copies the root manifest, so it carries
/// `[workspace.dependencies]` entries no selected member inherits and
/// `[profile.*.package.<name>]` overrides for packages the board never
/// resolves. Cargo warns about both on every invocation; drop them. Run once
/// Cargo has written the selected lock, which holds exactly the workspace
/// resolve Cargo checks profile overrides against.
pub(crate) fn prune_unused_entries(
    selected: &Path,
    metadata: &serde_json::Value,
) -> Result<(), String> {
    let lock = fs::read_to_string(selected.join("Cargo.lock"))
        .map_err(|e| format!("failed to read selected Cargo.lock: {e}"))?;
    let lock: toml::Table = toml::from_str(&lock).map_err(|e| e.to_string())?;
    let resolved: BTreeSet<&str> = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("selected Cargo.lock has no packages")?
        .iter()
        .filter_map(|package| package.get("name")?.as_str())
        .collect();
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("Cargo metadata has no workspace members")?;
    let mut inherited = BTreeSet::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata has no packages")?
        .iter()
        .filter(|package| members.contains(&package["id"]))
    {
        let path = package["manifest_path"]
            .as_str()
            .ok_or("package without manifest path")?;
        let manifest: toml::Table = toml::from_str(
            &fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?,
        )
        .map_err(|e| format!("{path}: {e}"))?;
        inherited.extend(inherited_dependencies(&manifest));
    }
    let manifest_path = selected.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read selected workspace manifest: {e}"))?;
    let pruned = without_entries(
        &manifest,
        |key| !inherited.contains(key),
        |package| !resolved.contains(package),
    )?;
    if pruned != manifest {
        crate::workspace::write(&manifest_path, pruned)?;
    }
    Ok(())
}

/// Dependency keys a member manifest inherits with `workspace = true`, from
/// every (target-specific) dependency table.
fn inherited_dependencies(manifest: &toml::Table) -> Vec<String> {
    let tables = |table: &toml::Table| -> Vec<toml::Table> {
        ["dependencies", "dev-dependencies", "build-dependencies"]
            .iter()
            .filter_map(|kind| table.get(*kind)?.as_table().cloned())
            .collect()
    };
    let targets = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values())
        .filter_map(toml::Value::as_table)
        .flat_map(tables);
    tables(manifest)
        .into_iter()
        .chain(targets)
        .flat_map(|dependencies| {
            dependencies
                .into_iter()
                .filter(|(_, spec)| {
                    spec.get("workspace").and_then(toml::Value::as_bool) == Some(true)
                })
                .map(|(key, _)| key)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Remove `[workspace.dependencies]` entries whose key matches and whole
/// `[profile.<profile>.package.<name>]` tables whose package name matches.
fn without_entries(
    manifest: &str,
    drop_dependency: impl Fn(&str) -> bool,
    drop_profile: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let mut value: toml::Table = toml::from_str(manifest).map_err(|e| e.to_string())?;
    if let Some(dependencies) = value
        .get_mut("workspace")
        .and_then(|v| v.get_mut("dependencies"))
        .and_then(toml::Value::as_table_mut)
    {
        dependencies.retain(|name, _| !drop_dependency(name));
    }
    if let Some(profiles) = value.get_mut("profile").and_then(toml::Value::as_table_mut) {
        for (_, profile) in profiles.iter_mut() {
            if let Some(packages) = profile
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
            {
                packages.retain(|name, _| !drop_profile(name));
            }
        }
    }
    toml::to_string(&value).map_err(|e| e.to_string())
}

pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

pub(crate) fn write_flat_binary(elf_path: &Path, bin_path: &Path) -> Result<(), String> {
    let elf_data = fs::read(elf_path)
        .map_err(|e| format!("failed to read ELF {}: {e}", elf_path.display()))?;
    let load_segments = elf_load_segments(&elf_data, elf_path)?;

    let min_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr)
        .min()
        .ok_or_else(|| format!("no loadable file-backed segments in {}", elf_path.display()))?;
    let max_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr.saturating_add(segment.filesz))
        .max()
        .unwrap_or(min_paddr);
    let len = max_paddr
        .checked_sub(min_paddr)
        .and_then(|len| usize::try_from(len).ok())
        .ok_or_else(|| {
            format!(
                "flat binary address range is too large in {}",
                elf_path.display()
            )
        })?;

    let mut flat = vec![0u8; len];
    for segment in load_segments.iter().filter(|segment| segment.filesz != 0) {
        let start = usize::try_from(segment.paddr - min_paddr)
            .map_err(|_| format!("segment offset is too large in {}", elf_path.display()))?;
        let size = usize::try_from(segment.filesz)
            .map_err(|_| format!("segment size is too large in {}", elf_path.display()))?;
        let file_start = usize::try_from(segment.offset)
            .map_err(|_| format!("segment file offset is too large in {}", elf_path.display()))?;
        let file_end = file_start
            .checked_add(size)
            .ok_or_else(|| format!("segment file range overflows in {}", elf_path.display()))?;
        if file_end > elf_data.len() {
            return Err(format!(
                "PT_LOAD at {:#x} extends past EOF in {}",
                segment.paddr,
                elf_path.display()
            ));
        }
        flat[start..start + size].copy_from_slice(&elf_data[file_start..file_end]);
    }

    fs::write(bin_path, flat)
        .map_err(|e| format!("failed to write flat binary {}: {e}", bin_path.display()))
}

#[derive(Debug, Clone, Copy)]
struct ElfLoadSegment {
    offset: u64,
    paddr: u64,
    filesz: u64,
}

fn elf_load_segments(elf_data: &[u8], elf_path: &Path) -> Result<Vec<ElfLoadSegment>, String> {
    if elf_data.len() < 16 {
        return Err(format!("ELF {} is too short", elf_path.display()));
    }

    match elf_data[4] {
        elf::ELFCLASS32 => elf_load_segments_for::<elf::FileHeader32<object::Endianness>>(elf_data),
        elf::ELFCLASS64 => elf_load_segments_for::<elf::FileHeader64<object::Endianness>>(elf_data),
        class => {
            return Err(format!(
                "unsupported ELF class {class} in {}",
                elf_path.display()
            ));
        }
    }
    .map_err(|e| format!("failed to parse ELF {}: {e}", elf_path.display()))
}

fn elf_load_segments_for<Elf>(elf_data: &[u8]) -> Result<Vec<ElfLoadSegment>, object::Error>
where
    Elf: FileHeader,
{
    let elf = ElfFile::<Elf>::parse(elf_data)?;
    let endian = elf.elf_header().endian()?;
    Ok(elf
        .elf_program_headers()
        .iter()
        .filter(|phdr| phdr.p_type(endian) == elf::PT_LOAD)
        .map(|phdr| ElfLoadSegment {
            offset: phdr.p_offset(endian).into(),
            paddr: phdr.p_paddr(endian).into(),
            filesz: phdr.p_filesz(endian).into(),
        })
        .collect())
}

fn workspace_root() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var("FSTART_WORKSPACE_ROOT") {
        return Ok(PathBuf::from(root));
    }

    let mut dir = std::env::current_dir().map_err(|e| format!("no cwd: {e}"))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unused_dependencies_and_unresolved_profile_overrides_are_dropped() {
        let manifest = "[workspace.dependencies]\n\
            fstart-core = { path = \"crates/core\" }\n\
            fstart-platform-sunxi = { path = \"crates/platform-sunxi\" }\n\
            object = { version = \"0.37\", features = [\n  \"elf\",\n] }\n\
            [profile.dev.package.fstart-core]\nopt-level = \"s\"\n\
            [profile.dev.package.fstart-platform-sunxi]\nopt-level = \"s\"\n\
            [profile.release]\nlto = true\n";
        assert_eq!(
            toml::from_str::<toml::Value>(
                &super::without_entries(
                    manifest,
                    |key| key == "object",
                    |package| package == "fstart-platform-sunxi"
                )
                .unwrap()
            )
            .unwrap(),
            toml::from_str::<toml::Value>(
                "[workspace.dependencies]\nfstart-core = { path = \"crates/core\" }\n\
             fstart-platform-sunxi = { path = \"crates/platform-sunxi\" }\n\
             [profile.dev.package.fstart-core]\nopt-level = \"s\"\n\
             [profile.release]\nlto = true\n"
            )
            .unwrap()
        );
    }

    #[test]
    fn inherited_dependencies_cover_every_dependency_table() {
        let manifest: toml::Table = toml::from_str(
            "[dependencies]\na = { workspace = true }\nb = \"1\"\n\
             [dev-dependencies]\nc.workspace = true\n\
             [target.'cfg(unix)'.build-dependencies]\nd = { workspace = true, features = [] }\n",
        )
        .unwrap();
        let mut keys = super::inherited_dependencies(&manifest);
        keys.sort();
        assert_eq!(keys, ["a", "c", "d"]);
    }
}
