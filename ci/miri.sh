#!/usr/bin/env bash
# Run Miri undefined-behavior checks on SMM and SMM-adjacent crates.
set -euo pipefail
cd "$(dirname "$0")/.."

# sccache does not wrap miri's custom rustc driver cleanly.
unset RUSTC_WRAPPER

# SmmEntryParams carries physical addresses as architectural `u64` fields across
# the assembly/Rust boundary; permissive provenance still checks Stacked Borrows,
# alignment, initialization, out-of-bounds accesses, and data races.
export MIRIFLAGS="-Zmiri-permissive-provenance ${MIRIFLAGS:-}"

cargo miri test --locked -p fstart-smm --all-features
cargo miri test --locked -p fstart-core --lib
cargo miri test --locked -p fstart-options --all-features --lib
cargo miri test --locked -p fstart-store --lib
cargo miri test --locked -p fstart-coreboot --lib
case "$(uname -m)" in
  x86_64|i?86)
    cargo miri test --locked -p fstart-arch --features x86_64 --lib -- \
      x86::cpu::amd::smm x86::cpu::intel::smrr
    ;;
  *)
    echo "note: skipping x86_64-only fstart-arch Miri tests on $(uname -m)" >&2
    ;;
esac
