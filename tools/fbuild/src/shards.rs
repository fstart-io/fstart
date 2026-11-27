//! Split the boards into the firmware jobs CI runs.
//!
//! Hosted runners are scarce and every job pays for its own toolchain, host
//! tools and empty Cargo target directory, so boards are built several to a
//! job: grouped by architecture, where they share core/alloc and most
//! dependencies, and split into shards of at most [`MAX_BUILDS`] builds. A
//! shard also boots its own boards in QEMU, reusing the target directory that
//! just built them.
//!
//! `fbuild shards` prints the job matrix `{"include": [{"shard", "boards",
//! "packages"}, ...]}`. `boards` is space separated board ids, `packages` the
//! apt packages that emulate the shard's architecture for whichever of its
//! boards have boot tests (their plans' run recipes say which), empty where
//! QEMU has no machine for it.

use crate::board_manifest::BoardManifest;
use serde::Serialize;
use std::collections::BTreeMap;

/// Builds per job: a board and each of its variants count as one build each.
///
/// Boards of the same platform share `core`, `alloc`, registry crates, and
/// workspace drivers in Cargo's target directory, so each additional board in
/// a shard only compiles and links its own board crate (~10–45 s). Keeping a
/// whole platform together avoids repeating runner setup and cold-cache
/// warm-up across multiple runners.
pub const MAX_BUILDS: usize = 24;

/// Platform crates whose boards declare QEMU boot tests. Shards with none of
/// these skip installing QEMU/swtpm and downloading the `boot-assets` artifact.
fn has_qemu_boot_tests(dependency: &str) -> bool {
    matches!(dependency, "fstart-platform-qemu" | "fstart-platform-sunxi")
}

/// The apt packages emulating an architecture. CI installs no recommended
/// packages, but the network cards of the virt machines need the option ROMs
/// of ipxe-qemu and sbsa-ref's display the VGA BIOS of seabios, which only
/// qemu-system-x86 depends on; the q35 measured-boot tests need swtpm. The
/// names are those of the runner image the workflow pins (Ubuntu 26.04).
fn qemu_packages(arch: &str, boards: &[Board<'_>]) -> &'static str {
    if !boards.iter().any(|b| has_qemu_boot_tests(b.dependency)) {
        return "";
    }
    match arch {
        "aarch64" => "qemu-system-arm ipxe-qemu seabios",
        "armv7" => "qemu-system-arm ipxe-qemu",
        "riscv64" => "qemu-system-riscv ipxe-qemu",
        "x86" => "qemu-system-x86",
        "x86_64" => "qemu-system-x86 swtpm swtpm-tools",
        _ => "",
    }
}

/// One firmware job.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Shard {
    pub shard: String,
    pub boards: String,
    pub packages: String,
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
                .chain(board.variants.keys().map(String::as_str))
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

fn shard_boards(shard: &[Board<'_>]) -> String {
    shard
        .iter()
        .flat_map(|b| b.builds.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

#[must_use]
pub fn matrix(boards: &[Board<'_>]) -> Matrix {
    let mut arches: BTreeMap<&str, Vec<Board<'_>>> = BTreeMap::new();
    for board in boards {
        arches.entry(board.arch).or_default().push(board.clone());
    }

    let mut include = Vec::new();
    for (arch, mut group) in arches {
        // Boards of one platform crate share the most code: keep them together.
        group.sort_by_key(|b| (b.dependency, b.board));
        let total_builds: usize = group.iter().map(|b| b.builds.len()).sum();
        if total_builds <= MAX_BUILDS {
            include.push(Shard {
                shard: arch.to_string(),
                boards: shard_boards(&group),
                packages: qemu_packages(arch, &group).into(),
            });
            continue;
        }

        // Large architectures split by platform crate first so each platform
        // warms its shared drivers once and non-QEMU platforms skip QEMU setup.
        let mut by_platform: BTreeMap<&str, Vec<Board<'_>>> = BTreeMap::new();
        for board in group {
            by_platform.entry(board.dependency).or_default().push(board);
        }
        for (dependency, platform_boards) in by_platform {
            let base = if dependency.is_empty() {
                arch.to_string()
            } else {
                let platform = dependency
                    .strip_prefix("fstart-platform-")
                    .unwrap_or(dependency);
                format!("{arch}-{platform}")
            };
            let shards = split(&platform_boards);
            for (index, shard) in shards.iter().enumerate() {
                include.push(Shard {
                    shard: if shards.len() == 1 {
                        base.clone()
                    } else {
                        format!("{base}-{}", index + 1)
                    },
                    boards: shard_boards(shard),
                    packages: qemu_packages(arch, shard).into(),
                });
            }
        }
    }
    Matrix { include }
}

/// `fbuild shards`: the matrix of every discovered board.
pub fn run() -> Result<(), String> {
    let root = crate::build_board::workspace_root()?;
    let manifests = crate::board_manifest::discover(&root)?;
    let boards: Vec<Board<'_>> = manifests.iter().map(Board::from).collect();
    let matrix = matrix(&boards);
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

    fn shard(shard: &str, boards: &str, packages: &str) -> Shard {
        Shard {
            shard: shard.into(),
            boards: boards.into(),
            packages: packages.into(),
        }
    }

    #[test]
    fn one_shard_per_arch_with_its_emulator() {
        let boards = [
            board("qemu-riscv64", "riscv64", "fstart-platform-qemu"),
            board("qemu-q35", "x86_64", "fstart-platform-qemu"),
            board("unmatched", "riscv64", "fstart-platform-sifive"),
        ];
        assert_eq!(
            matrix(&boards).include,
            [
                shard(
                    "riscv64",
                    "qemu-riscv64 unmatched",
                    "qemu-system-riscv ipxe-qemu",
                ),
                shard("x86_64", "qemu-q35", "qemu-system-x86 swtpm swtpm-tools"),
            ]
        );
    }

    #[test]
    fn an_arch_qemu_cannot_emulate_installs_nothing() {
        let include = matrix(&[board("bmc", "armv5te", "")]).include;
        assert_eq!(include, [shard("armv5te", "bmc", "")]);
    }

    #[test]
    fn large_arch_splits_by_platform_crate_and_scopes_qemu_packages() {
        let names: Vec<_> = (0..MAX_BUILDS).map(|i| format!("intel-{i:02}")).collect();
        let mut boards: Vec<_> = names
            .iter()
            .map(|n| board(n, "x86_64", "fstart-platform-intel"))
            .collect();
        boards.push(Board {
            builds: vec!["amd", "amd-b", "amd-c"],
            ..board("amd", "x86_64", "fstart-platform-amd")
        });
        boards.push(board("qemu-q35", "x86_64", "fstart-platform-qemu"));
        let include = matrix(&boards).include;
        let summary: Vec<_> = include
            .iter()
            .map(|s| (s.shard.as_str(), s.packages.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("x86_64-amd", ""),
                ("x86_64-intel", ""),
                ("x86_64-qemu", "qemu-system-x86 swtpm swtpm-tools"),
            ]
        );
        assert_eq!(include[0].boards, "amd amd-b amd-c");
        assert_eq!(include[1].boards, names.join(" "));
        assert_eq!(include[2].boards, "qemu-q35");
    }

    #[test]
    fn oversized_platform_splits_without_separating_variants() {
        let names: Vec<_> = (0..MAX_BUILDS).map(|i| format!("intel-{i:02}")).collect();
        let mut boards: Vec<_> = names
            .iter()
            .map(|n| board(n, "x86_64", "fstart-platform-intel"))
            .collect();
        boards.push(Board {
            builds: vec!["z-var", "z-var-b"],
            ..board("z-var", "x86_64", "fstart-platform-intel")
        });
        let include = matrix(&boards).include;
        let shards: Vec<_> = include.iter().map(|s| s.shard.as_str()).collect();
        assert_eq!(shards, ["x86_64-intel-1", "x86_64-intel-2"]);
        assert_eq!(include[0].boards, names.join(" "));
        assert_eq!(include[1].boards, "z-var z-var-b");
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
        let include = matrix(&boards).include;
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
        let include = matrix(&boards).include;
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
}
