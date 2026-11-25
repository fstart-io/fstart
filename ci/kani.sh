#!/usr/bin/env bash
# Run Kani formal verification proofs for SMM isolation, layout, header,
# rendezvous, hardware TSEG/SMRR register math, and the FFS boot path: locator
# and trust blocks, the signed boot root, the directory, LZ4 and the loader.
#
# Harnesses are listed explicitly with --exact so a broken cfg gate or
# accidental rename can never silently drop a proof.
set -euo pipefail
cd "$(dirname "$0")/.."

# sccache cannot wrap kani-compiler.
unset RUSTC_WRAPPER

smm_harnesses=(
  comm::proofs::verify_memory_range_overlaps_matches_u128_spec
  comm::proofs::verify_memory_range_subrange_matches_u128_spec
  comm::proofs::verify_policy_rejects_any_overlap_with_smram_or_mmio
  comm::proofs::verify_policy_forbidden_overrides_allowed_and_subrange_stays_isolated
  layout::proofs::verify_common_and_page_table_layout_inside_smram
  layout::proofs::verify_cpu_layout_containment_and_pairwise_disjointness
  header::proofs::verify_header_parse_enforces_file_and_memory_bounds
  header::proofs::verify_header_entry_and_fixup_never_panic_and_bound_results
  handler::proofs::verify_rendezvous_claim_enforces_mutual_exclusion
  handler::proofs::verify_rendezvous_acknowledge_preserves_generation_and_monotonicity
)

arch_harnesses=(
  x86::cpu::amd::smm::proofs::verify_valid_tseg_matches_u128_spec
  x86::cpu::amd::smm::proofs::verify_smm_mask_matches_exact_tseg_address_range
  x86::cpu::amd::smm::proofs::verify_free_range_is_page_aligned_and_bounded
  x86::cpu::intel::smrr::proofs::verify_smrr_range_matches_exact_tseg_address_range
)

core_harnesses=(
  ffs::locator::proofs::verify_media_locator_validate_matches_u128_spec
  ffs::locator::proofs::verify_locator_block_parse_round_trips
  ffs::trust::proofs::verify_trust_block_parse_round_trips_and_bounds_keys
)

ffs_harnesses=(
  root::proofs::verify_bootstrap_descriptor_parse_encode_round_trip
  root::proofs::verify_bootstrap_descriptor_validate_encode_parse_round_trip
  root::proofs::verify_directory_ref_parse_validate_encode_round_trip
  root::proofs::verify_root_parse_enforces_layout_and_round_trips
  root::proofs::verify_root_parse_rejects_wrong_length
  lz4::proofs::verify_copy_match_short_meets_spec
  lz4::proofs::verify_copy_match_careful_meets_spec
  lz4::proofs::verify_wild_copy_within_is_sound_and_meets_spec
  lz4::proofs::verify_wild_copy_meets_spec
  lz4::proofs::verify_read_length_extension_meets_spec
  lz4::proofs::verify_decode_sequence_matches_reference
  lz4::proofs::verify_decode_sequence_with_literal_extension_matches_reference
  directory::proofs::verify_string_at_is_bounded_and_nul_terminated
  directory::proofs::verify_from_utf8_ascii_matches_core
  directory::proofs::verify_directory_parse_never_panics_on_arbitrary_bytes
  directory::proofs::verify_directory_find_file_by_type_is_exact
  directory::proofs::verify_directory_find_file_by_name_is_exact
  directory::proofs::verify_directory_raw_region_is_exact
  directory::proofs::verify_directory_validate_regions_enforces_unique_contained_names
  directory::proofs::verify_directory_validate_partition_gives_unique_owners
)

# These harnesses bound loops over symbolic-length tables (two-record
# directory tables, one-byte names, up to four policy keys) with a small
# harness-wide unwind, yet also compare fixed byte arrays (32-byte digests,
# 160-byte descriptor slots). CBMC lowers those compares to its builtin
# memcmp, whose loop needs one unwinding more than the longest array; raising
# the bound for memcmp alone keeps every unwinding assertion enabled.
ffs_digest_harnesses=(
  root::proofs::verify_authenticate_root_enforces_policy
  directory::proofs::verify_directory_validate_files_bounds_segments
  directory::proofs::verify_directory_validate_segments_enforces_segment_rules
)

load_harnesses=(
  proofs::verify_memory_window_matches_u128_spec
  proofs::verify_memory_policy_permits_only_writable_unreserved_ranges
  proofs::verify_bootstrap_footprint_contains_disjoint_output_and_input
)

smm_args=()
for h in "${smm_harnesses[@]}"; do
  smm_args+=(--harness "$h")
done

arch_args=()
for h in "${arch_harnesses[@]}"; do
  arch_args+=(--harness "$h")
done

cargo kani -p fstart-smm --exact "${smm_args[@]}"
# fstart-arch contains unrelated boot-entry global_asm! blocks that none of the
# TSEG/SMRR arithmetic harnesses reach.
cargo kani -Z unstable-options --ignore-global-asm \
  -p fstart-arch --features x86_64 --exact "${arch_args[@]}"

# The FFS harnesses are independent and take minutes each; run them in parallel.
jobs=(-j "${KANI_JOBS:-$(nproc)}" --output-format=terse)
core_args=()
for h in "${core_harnesses[@]}"; do
  core_args+=(--harness "$h")
done
ffs_args=()
for h in "${ffs_harnesses[@]}"; do
  ffs_args+=(--harness "$h")
done
ffs_digest_args=()
for h in "${ffs_digest_harnesses[@]}"; do
  ffs_digest_args+=(--harness "$h")
done
load_args=()
for h in "${load_harnesses[@]}"; do
  load_args+=(--harness "$h")
done

cargo kani -p fstart-core --exact "${jobs[@]}" "${core_args[@]}"
# Root authentication stubs signature verification (recording the call), LZ4
# stubs copy helpers by their proven specifications, and directory proofs stub
# UTF-8 decoding of one-byte ASCII names (proven equal to core) and table
# validation where proven apart.
cargo kani -Z stubbing -p fstart-ffs --features directory --exact "${jobs[@]}" \
  "${ffs_args[@]}"
cargo kani -Z stubbing -Z unstable-options -p fstart-ffs --features directory \
  --exact "${jobs[@]}" "${ffs_digest_args[@]}" --cbmc-args --unwindset memcmp.0:161
cargo kani -p fstart-load --exact "${jobs[@]}" "${load_args[@]}"
