//! Split the boards into the firmware jobs CI runs.
//!
//! Hosted runners are scarce and every job pays for its own toolchain, host
//! tools and empty Cargo target directory, so boards are built several to a
//! job: grouped by architecture, where they share core/alloc and most
//! dependencies, and split into shards of at most [`MAX_BUILDS`] builds. A
//! shard also boots its own boards in QEMU, reusing the target directory that
//! just built them.
//!
//! ```sh
//! ci/qemu-boot-tests.sh --list | cargo fbuild shards
//! ```
//!
//! prints the job matrix `{"include": [{"shard", "boards", "boots", "qemu",
//! "assets"}, ...]}`. `boards` and `boots` are space separated board ids;
//! `qemu` is the apt packages emulating the shard's architecture and `assets`
//! the boot-assets artifact its boots read, both empty without boots.

use crate::board_manifest::BoardManifest;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Builds per job: a board and each of its variants count as one build each.
pub const MAX_BUILDS: usize = 7;

/// The apt packages emulating a platform. CI installs no recommended
/// packages, but the network cards of the virt machines need the option ROMs
/// of ipxe-qemu and sbsa-ref's display the VGA BIOS of seabios, which only
/// qemu-system-x86 depends on. The names are those of the runner image the
/// workflow pins (Ubuntu 26.04).
fn qemu_packages(platform: &str) -> Option<&'static str> {
    Some(match platform {
        "aarch64" => "qemu-system-arm ipxe-qemu seabios",
        "armv7" => "qemu-system-arm ipxe-qemu",
        "riscv64" => "qemu-system-riscv ipxe-qemu",
        "x86" | "x86_64" => "qemu-system-x86",
        _ => return None,
    })
}

/// One firmware job.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Shard {
    pub shard: String,
    pub boards: String,
    pub boots: String,
    pub qemu: String,
    pub assets: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Matrix {
    pub include: Vec<Shard>,
}

/// What sharding needs of a board.
#[derive(Debug, Clone)]
pub struct Board<'a> {
    pub board: &'a str,
    /// The architecture shard; boards that declare none share `other`.
    pub arch: &'a str,
    /// The platform crate, empty when the board declares no profile.
    pub dependency: &'a str,
    /// The board and its variants.
    pub builds: Vec<&'a str>,
}

impl<'a> From<&'a BoardManifest> for Board<'a> {
    fn from(board: &'a BoardManifest) -> Self {
        Self {
            board: &board.board,
            arch: board.arch.as_deref().unwrap_or("other"),
            dependency: board
                .build_profile
                .as_ref()
                .map_or("", |profile| &profile.dependency),
            builds: std::iter::once(board.board.as_str())
                .chain(board.variants.iter().map(|(id, ..)| id.as_str()))
                .collect(),
        }
    }
}

/// Consecutive runs of boards with at most [`MAX_BUILDS`] builds each.
fn split<'a, 'b>(boards: &'b [Board<'a>]) -> Vec<&'b [Board<'a>]> {
    let mut shards = Vec::new();
    let (mut start, mut count) = (0, 0);
    for (index, board) in boards.iter().enumerate() {
        if index > start && count + board.builds.len() > MAX_BUILDS {
            shards.push(&boards[start..index]);
            (start, count) = (index, 0);
        }
        count += board.builds.len();
    }
    if start < boards.len() {
        shards.push(&boards[start..]);
    }
    shards
}

pub fn matrix(boards: &[Board<'_>], boot_boards: &BTreeSet<&str>) -> Result<Matrix, String> {
    let known: BTreeSet<_> = boards.iter().map(|b| b.board).collect();
    let unknown: Vec<_> = boot_boards.difference(&known).copied().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "boot tests name unknown boards: {}",
            unknown.join(", ")
        ));
    }

    let mut arches: BTreeMap<&str, Vec<Board<'_>>> = BTreeMap::new();
    for board in boards {
        arches.entry(board.arch).or_default().push(board.clone());
    }

    let mut include = Vec::new();
    for (arch, mut group) in arches {
        // Boards of one platform crate share the most code: keep them together.
        group.sort_by_key(|b| (b.dependency, b.board));
        let shards = split(&group);
        for (index, shard) in shards.iter().enumerate() {
            let boots: Vec<_> = shard
                .iter()
                .map(|b| b.board)
                .filter(|b| boot_boards.contains(b))
                .collect();
            // Every board is built; only one that boots needs an emulator.
            let (qemu, assets) = if boots.is_empty() {
                ("", "")
            } else {
                let qemu = qemu_packages(arch).ok_or_else(|| {
                    format!(
                        "no QEMU package known for arch {arch}, which {} boot on",
                        boots.join(", ")
                    )
                })?;
                let assets = if matches!(arch, "x86" | "x86_64") {
                    "x86_64"
                } else {
                    "payloads"
                };
                (qemu, assets)
            };
            include.push(Shard {
                shard: if shards.len() == 1 {
                    arch.to_string()
                } else {
                    format!("{arch}-{}", index + 1)
                },
                boards: shard
                    .iter()
                    .flat_map(|b| b.builds.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" "),
                boots: boots.join(" "),
                qemu: qemu.into(),
                assets: assets.into(),
            });
        }
    }
    Ok(Matrix { include })
}

/// `fbuild shards`: the matrix of every discovered board, with the boots
/// `qemu-boot-tests.sh --list` writes to `boot_list` ("board payload" lines).
pub fn run(boot_list: &str) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let manifests = crate::board_manifest::discover(&root)?;
    let boards: Vec<Board<'_>> = manifests.iter().map(Board::from).collect();
    let boots = boot_list
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    let matrix = matrix(&boards, &boots)?;
    println!(
        "{}",
        serde_json::to_string(&matrix).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board<'a>(name: &'a str, arch: &'a str, dependency: &'a str) -> Board<'a> {
        Board {
            board: name,
            arch,
            dependency,
            builds: vec![name],
        }
    }

    fn shard(shard: &str, boards: &str, boots: &str, qemu: &str, assets: &str) -> Shard {
        Shard {
            shard: shard.into(),
            boards: boards.into(),
            boots: boots.into(),
            qemu: qemu.into(),
            assets: assets.into(),
        }
    }

    #[test]
    fn one_shard_per_platform_with_its_boots() {
        let boards = [
            board("qemu-riscv64", "riscv64", "fstart-platform-qemu"),
            board("qemu-q35", "x86_64", "fstart-platform-qemu"),
            board("unmatched", "riscv64", "fstart-platform-sifive"),
        ];
        let boots = BTreeSet::from(["qemu-q35", "qemu-riscv64"]);
        assert_eq!(
            matrix(&boards, &boots).unwrap().include,
            [
                shard(
                    "riscv64",
                    "qemu-riscv64 unmatched",
                    "qemu-riscv64",
                    "qemu-system-riscv ipxe-qemu",
                    "payloads"
                ),
                shard(
                    "x86_64",
                    "qemu-q35",
                    "qemu-q35",
                    "qemu-system-x86",
                    "x86_64"
                ),
            ]
        );
    }

    #[test]
    fn shard_without_boots_installs_no_qemu() {
        let include = matrix(&[board("pc2", "aarch64", "")], &BTreeSet::new())
            .unwrap()
            .include;
        assert_eq!(include, [shard("aarch64", "pc2", "", "", "")]);
    }

    #[test]
    fn large_platform_splits_by_platform_crate_with_variants_kept() {
        let names: Vec<_> = (0..6).map(|i| format!("intel-{i}")).collect();
        let mut boards: Vec<_> = names
            .iter()
            .map(|n| board(n, "x86_64", "fstart-platform-intel"))
            .collect();
        boards.push(Board {
            builds: vec!["amd", "amd-b", "amd-c"],
            ..board("amd", "x86_64", "fstart-platform-amd")
        });
        let include = matrix(&boards, &BTreeSet::new()).unwrap().include;
        let shards: Vec<_> = include.iter().map(|s| s.shard.as_str()).collect();
        assert_eq!(shards, ["x86_64-1", "x86_64-2"]);
        // The AMD board sorts first; its variants count against the limit
        // and are never separated from it.
        assert_eq!(
            include[0].boards,
            "amd amd-b amd-c intel-0 intel-1 intel-2 intel-3"
        );
        assert_eq!(include[1].boards, "intel-4 intel-5");
    }

    #[test]
    fn every_build_is_in_exactly_one_shard() {
        let platforms = ["x86_64"; 11]
            .into_iter()
            .chain(["x86"; 4])
            .chain(["armv7"; 2]);
        let names: Vec<_> = (0..17)
            .map(|i| (format!("b{i}"), format!("b{i}-v")))
            .collect();
        let boards: Vec<_> = names
            .iter()
            .zip(platforms)
            .enumerate()
            .map(|(i, ((name, variant), platform))| Board {
                builds: if i % 3 == 0 {
                    vec![name, variant]
                } else {
                    vec![name]
                },
                ..board(name, platform, "")
            })
            .collect();
        let include = matrix(&boards, &BTreeSet::new()).unwrap().include;
        let mut built: Vec<_> = include.iter().flat_map(|s| s.boards.split(' ')).collect();
        let mut expected: Vec<_> = boards.iter().flat_map(|b| b.builds.clone()).collect();
        built.sort_unstable();
        expected.sort_unstable();
        assert_eq!(built, expected);
    }

    #[test]
    fn boards_without_metadata_are_still_built() {
        let boards = [
            board("bare", "other", ""),
            board("novel", "ppc64", "fstart-platform-qemu"),
            board("qemu-q35", "x86_64", "fstart-platform-qemu"),
        ];
        let include = matrix(&boards, &BTreeSet::new()).unwrap().include;
        let shards: Vec<_> = include
            .iter()
            .map(|s| (s.shard.as_str(), s.boards.as_str()))
            .collect();
        assert_eq!(
            shards,
            [
                ("other", "bare"),
                ("ppc64", "novel"),
                ("x86_64", "qemu-q35")
            ]
        );
    }

    #[test]
    fn boot_on_a_platform_without_qemu_packages_is_an_error() {
        let error = matrix(&[board("novel", "ppc64", "")], &BTreeSet::from(["novel"]));
        assert!(error.unwrap_err().contains("ppc64, which novel boot"));
    }

    #[test]
    fn boot_test_of_an_unknown_board_is_an_error() {
        let error = matrix(
            &[board("qemu-q35", "x86_64", "")],
            &BTreeSet::from(["gone"]),
        );
        assert!(error.unwrap_err().contains("gone"));
    }
}
