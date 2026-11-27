//! Kani formal verification proof runner (`fbuild kani`).
//!
//! Harnesses are listed explicitly with `--exact` so a broken `cfg` gate or
//! accidental rename can never silently drop a proof. Harnesses are grouped by
//! `(package, features, flags)` so `kani-compiler` and `goto-cc` run once per
//! group, and the two heavy `fstart-ffs` streams run concurrently in separate
//! `--target-dir`s so the long single-harness tail in `fstart-ffs` digest
//! proofs overlaps with the main `fstart-ffs` suite instead of leaving cores
//! idle.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One batched `cargo kani` invocation sharing a crate, feature set, and flags.
#[derive(Debug, Clone, Copy)]
pub struct ProofGroup {
    pub name: &'static str,
    pub package: &'static str,
    pub features: &'static [&'static str],
    pub kani_args: &'static [&'static str],
    pub cbmc_args: &'static [&'static str],
    pub harnesses: &'static [&'static str],
}

const ARCH_KANI_ARGS: &[&str] = &["-Z", "unstable-options", "--ignore-global-asm"];
const ARCH_FEATURES: &[&str] = &["x86_64"];
const FFS_KANI_ARGS: &[&str] = &["-Z", "stubbing"];
const FFS_DIGEST_KANI_ARGS: &[&str] = &["-Z", "stubbing", "-Z", "unstable-options"];
const FFS_FEATURES: &[&str] = &["directory"];
// These harnesses bound loops over symbolic-length tables (two-record
// directory tables, one-byte names, up to four policy keys) with a small
// harness-wide unwind, yet also compare fixed byte arrays (32-byte digests,
// 160-byte descriptor slots). CBMC lowers those compares to its builtin
// memcmp, whose loop needs one unwinding more than the longest array; raising
// the bound for memcmp alone keeps every unwinding assertion enabled.
const FFS_DIGEST_CBMC_ARGS: &[&str] = &["--unwindset", "memcmp.0:161"];

/// Main `fstart-ffs` proof group (ordered heaviest-first so Kani's `-j`
/// scheduler starts the longest proofs at `t = 0`).
pub const FFS_GROUP: ProofGroup = ProofGroup {
    name: "fstart-ffs",
    package: "fstart-ffs",
    features: FFS_FEATURES,
    kani_args: FFS_KANI_ARGS,
    cbmc_args: &[],
    harnesses: &[
        "directory::proofs::verify_directory_validate_regions_enforces_unique_contained_names",
        "lz4::proofs::verify_wild_copy_meets_spec",
        "directory::proofs::verify_directory_find_file_by_name_is_exact",
        "lz4::proofs::verify_wild_copy_within_is_sound_and_meets_spec",
        "lz4::proofs::verify_copy_match_short_meets_spec",
        "lz4::proofs::verify_decode_sequence_matches_reference",
        "root::proofs::verify_root_parse_enforces_layout_and_round_trips",
        "lz4::proofs::verify_decode_sequence_with_literal_extension_matches_reference",
        "directory::proofs::verify_string_at_is_bounded_and_nul_terminated",
        "directory::proofs::verify_directory_parse_never_panics_on_arbitrary_bytes",
        "directory::proofs::verify_directory_find_file_by_type_is_exact",
        "lz4::proofs::verify_copy_match_careful_meets_spec",
        "directory::proofs::verify_directory_validate_partition_gives_unique_owners",
        "directory::proofs::verify_directory_raw_region_is_exact",
        "root::proofs::verify_bootstrap_descriptor_validate_encode_parse_round_trip",
        "root::proofs::verify_root_parse_rejects_wrong_length",
        "root::proofs::verify_bootstrap_descriptor_parse_encode_round_trip",
        "root::proofs::verify_directory_ref_parse_validate_encode_round_trip",
        "directory::proofs::verify_from_utf8_ascii_matches_core",
        "lz4::proofs::verify_read_length_extension_meets_spec",
    ],
};

/// Secondary stream: runs concurrently with [`FFS_GROUP`] in its own
/// `--target-dir` so the ~186s digest proof overlaps with the ~206s LZ4/dir
/// proofs, followed by the smaller crates.
pub const OTHER_GROUPS: &[ProofGroup] = &[
    ProofGroup {
        name: "fstart-ffs-digest",
        package: "fstart-ffs",
        features: FFS_FEATURES,
        kani_args: FFS_DIGEST_KANI_ARGS,
        cbmc_args: FFS_DIGEST_CBMC_ARGS,
        harnesses: &[
            "root::proofs::verify_authenticate_root_enforces_policy",
            "directory::proofs::verify_directory_validate_segments_enforces_segment_rules",
            "directory::proofs::verify_directory_validate_files_bounds_segments",
        ],
    },
    ProofGroup {
        name: "fstart-smm",
        package: "fstart-smm",
        features: &[],
        kani_args: &[],
        cbmc_args: &[],
        harnesses: &[
            "layout::proofs::verify_cpu_layout_containment_and_pairwise_disjointness",
            "header::proofs::verify_header_parse_enforces_file_and_memory_bounds",
            "header::proofs::verify_header_entry_and_fixup_never_panic_and_bound_results",
            "comm::proofs::verify_policy_forbidden_overrides_allowed_and_subrange_stays_isolated",
            "layout::proofs::verify_common_and_page_table_layout_inside_smram",
            "comm::proofs::verify_policy_rejects_any_overlap_with_smram_or_mmio",
            "handler::proofs::verify_rendezvous_acknowledge_preserves_generation_and_monotonicity",
            "handler::proofs::verify_rendezvous_claim_enforces_mutual_exclusion",
            "comm::proofs::verify_memory_range_overlaps_matches_u128_spec",
            "comm::proofs::verify_memory_range_subrange_matches_u128_spec",
        ],
    },
    ProofGroup {
        name: "fstart-arch",
        package: "fstart-arch",
        features: ARCH_FEATURES,
        kani_args: ARCH_KANI_ARGS,
        cbmc_args: &[],
        harnesses: &[
            "x86::cpu::amd::smm::proofs::verify_free_range_is_page_aligned_and_bounded",
            "x86::cpu::amd::smm::proofs::verify_smm_mask_matches_exact_tseg_address_range",
            "x86::cpu::amd::smm::proofs::verify_valid_tseg_matches_u128_spec",
            "x86::cpu::intel::smrr::proofs::verify_smrr_range_matches_exact_tseg_address_range",
        ],
    },
    ProofGroup {
        name: "fstart-core",
        package: "fstart-core",
        features: &[],
        kani_args: &[],
        cbmc_args: &[],
        harnesses: &[
            "ffs::trust::proofs::verify_trust_block_parse_round_trips_and_bounds_keys",
            "ffs::locator::proofs::verify_locator_block_parse_round_trips",
            "ffs::locator::proofs::verify_media_locator_validate_matches_u128_spec",
        ],
    },
    ProofGroup {
        name: "fstart-load",
        package: "fstart-load",
        features: &[],
        kani_args: &[],
        cbmc_args: &[],
        harnesses: &[
            "proofs::verify_bootstrap_footprint_contains_disjoint_output_and_input",
            "proofs::verify_memory_window_matches_u128_spec",
            "proofs::verify_memory_policy_permits_only_writable_unreserved_ranges",
        ],
    },
];

#[cfg(test)]
fn all_groups() -> impl Iterator<Item = ProofGroup> {
    std::iter::once(FFS_GROUP).chain(OTHER_GROUPS.iter().copied())
}

fn matching_harnesses(group: &ProofGroup, filters: &[String]) -> Vec<&'static str> {
    group
        .harnesses
        .iter()
        .copied()
        .filter(|h| {
            filters.is_empty()
                || filters
                    .iter()
                    .any(|f| group.package.contains(f.as_str()) || h.contains(f.as_str()))
        })
        .collect()
}

fn group_command(
    root: &Path,
    target_dir: &Path,
    group: &ProofGroup,
    harnesses: &[&str],
    jobs: usize,
) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .arg("kani")
        .args(group.kani_args)
        .args(["-p", group.package]);
    if !group.features.is_empty() {
        cmd.args(["--features", &group.features.join(",")]);
    }
    cmd.arg("--target-dir").arg(target_dir).args([
        "--exact",
        "-j",
        &jobs.max(1).to_string(),
        "--output-format=terse",
    ]);
    for harness in harnesses {
        cmd.args(["--harness", harness]);
    }
    if !group.cbmc_args.is_empty() {
        cmd.arg("--cbmc-args").args(group.cbmc_args);
    }
    // sccache cannot wrap kani-compiler.
    cmd.env_remove("RUSTC_WRAPPER");
    cmd
}

struct GroupOutcome {
    name: &'static str,
    count: usize,
    elapsed: Duration,
    error: Option<String>,
}

fn run_group(
    root: &Path,
    target_dir: &Path,
    group: &ProofGroup,
    harnesses: &[&str],
    jobs: usize,
    print_lock: &Mutex<()>,
) -> GroupOutcome {
    let start = Instant::now();
    let output = group_command(root, target_dir, group, harnesses, jobs).output();
    let elapsed = start.elapsed();
    let error = match output {
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            Some(format!(
                "exited with {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
                out.status
            ))
        }
        Err(e) => Some(format!("failed to launch cargo kani: {e}")),
    };
    {
        let _guard = print_lock.lock().unwrap_or_else(|e| e.into_inner());
        match &error {
            None => println!(
                "PASS {} ({} harness(es), {:.1}s)",
                group.name,
                harnesses.len(),
                elapsed.as_secs_f64()
            ),
            Some(detail) => eprintln!(
                "FAIL {} ({} harness(es), {:.1}s): {detail}",
                group.name,
                harnesses.len(),
                elapsed.as_secs_f64()
            ),
        }
    }
    GroupOutcome {
        name: group.name,
        count: harnesses.len(),
        elapsed,
        error,
    }
}

/// Run the Kani proof suite (or list it with `list`).
pub fn run(jobs: Option<usize>, list: bool, filters: &[String]) -> Result<(), String> {
    let ffs_selected = matching_harnesses(&FFS_GROUP, filters);
    let other_selected: Vec<(ProofGroup, Vec<&'static str>)> = OTHER_GROUPS
        .iter()
        .copied()
        .filter_map(|g| {
            let h = matching_harnesses(&g, filters);
            (!h.is_empty()).then_some((g, h))
        })
        .collect();
    let total_harnesses =
        ffs_selected.len() + other_selected.iter().map(|(_, h)| h.len()).sum::<usize>();
    if total_harnesses == 0 {
        return Err(format!("no Kani harnesses match {filters:?}"));
    }
    if list {
        for h in &ffs_selected {
            println!("{} {h}", FFS_GROUP.package);
        }
        for (g, hs) in &other_selected {
            for h in hs {
                println!("{} {h}", g.package);
            }
        }
        return Ok(());
    }

    let root = crate::build_board::workspace_root()?;
    let total_jobs = jobs
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
        })
        .max(1);
    let suite_start = Instant::now();
    let print_lock = Mutex::new(());
    let ffs_target: PathBuf = root.join("target/kani/ffs");
    let other_target: PathBuf = root.join("target/kani/other");

    let run_both = !ffs_selected.is_empty() && !other_selected.is_empty() && total_jobs >= 2;
    let outcomes: Vec<GroupOutcome> = if run_both {
        // `fstart-ffs-digest` spends most of its time on a single ~186s
        // harness, so give `ffs` `total_jobs - 1` workers to keep all cores
        // busy during that tail while still letting `other` parallelize its
        // shorter harnesses.
        let other_jobs = (total_jobs / 2).clamp(1, 2);
        let ffs_jobs = (total_jobs - 1).max(1);
        println!(
            "Running {total_harnesses} Kani proof(s) in 2 concurrent streams (ffs: -j {ffs_jobs}, other: -j {other_jobs})..."
        );
        std::thread::scope(|scope| {
            let other_handle = scope.spawn(|| {
                other_selected
                    .iter()
                    .map(|(g, hs)| run_group(&root, &other_target, g, hs, other_jobs, &print_lock))
                    .collect::<Vec<_>>()
            });
            let ffs_outcome = run_group(
                &root,
                &ffs_target,
                &FFS_GROUP,
                &ffs_selected,
                ffs_jobs,
                &print_lock,
            );
            let mut all = vec![ffs_outcome];
            all.extend(other_handle.join().unwrap_or_else(|_| {
                vec![GroupOutcome {
                    name: "other",
                    count: 0,
                    elapsed: Duration::ZERO,
                    error: Some("Kani worker thread panicked".into()),
                }]
            }));
            all
        })
    } else {
        println!("Running {total_harnesses} Kani proof(s) with -j {total_jobs}...");
        let mut all = Vec::new();
        if !ffs_selected.is_empty() {
            all.push(run_group(
                &root,
                &ffs_target,
                &FFS_GROUP,
                &ffs_selected,
                total_jobs,
                &print_lock,
            ));
        }
        for (g, hs) in &other_selected {
            all.push(run_group(
                &root,
                &other_target,
                g,
                hs,
                total_jobs,
                &print_lock,
            ));
        }
        all
    };

    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|o| o.error.is_some())
        .map(|o| o.name)
        .collect();
    let passed_harnesses: usize = outcomes
        .iter()
        .filter(|o| o.error.is_none())
        .map(|o| o.count)
        .sum();
    println!(
        "\nKani summary: {passed_harnesses}/{total_harnesses} harness(es) passed in {:.1}s:",
        suite_start.elapsed().as_secs_f64()
    );
    for o in &outcomes {
        let status = if o.error.is_none() { "PASS" } else { "FAIL" };
        println!(
            "  {status}  {:6.1}s  {} ({} harness(es))",
            o.elapsed.as_secs_f64(),
            o.name,
            o.count
        );
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("Kani proof group(s) failed: {}", failed.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harnesses_are_unique_across_all_groups() {
        let mut unique = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for group in all_groups() {
            for &harness in group.harnesses {
                assert!(
                    unique.insert((group.package, harness)),
                    "duplicate Kani harness: {}::{harness}",
                    group.package
                );
                total += 1;
            }
        }
        assert_eq!(total, 43);
    }

    #[test]
    fn command_batches_harnesses_with_exact_flags_and_unsets_rustc_wrapper() {
        let root = Path::new("/workspace");
        let target = Path::new("/workspace/target/kani/other");
        let digest = OTHER_GROUPS
            .iter()
            .find(|g| g.name == "fstart-ffs-digest")
            .unwrap();
        let cmd = group_command(root, target, digest, digest.harnesses, 2);
        let args: Vec<&str> = cmd
            .get_args()
            .map(|a| a.to_str().expect("UTF-8 arg"))
            .collect();
        assert!(args.contains(&"--exact"));
        assert!(args.contains(&"-j"));
        assert!(args.contains(&"--target-dir"));
        assert!(args.contains(&"/workspace/target/kani/other"));
        assert!(args.contains(&"--cbmc-args"));
        assert!(args.contains(&"memcmp.0:161"));
        assert_eq!(
            args.iter().filter(|&&a| a == "--harness").count(),
            digest.harnesses.len()
        );
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "RUSTC_WRAPPER" && v.is_none())
        );

        let ffs_cmd = group_command(root, target, &FFS_GROUP, FFS_GROUP.harnesses, 3);
        let ffs_args: Vec<&str> = ffs_cmd
            .get_args()
            .map(|a| a.to_str().expect("UTF-8 arg"))
            .collect();
        assert!(!ffs_args.contains(&"--cbmc-args"));
    }
}
