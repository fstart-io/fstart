#!/usr/bin/env bash
# Run Kani formal verification proofs for SMM isolation, layout, header,
# rendezvous, and hardware TSEG/SMRR register math.
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
