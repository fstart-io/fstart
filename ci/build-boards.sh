#!/usr/bin/env bash
# Build boards in release and run their host library tests, one after the
# other in the same Cargo target directory. A failing board never stops the
# ones after it; the script fails at the end if any did.
#
# Usage: ci/build-boards.sh SIZE_DIR BOARD...
#
# SIZE_DIR receives one stage-sizes-BOARD.jsonl per board (see
# `fbuild sizes`). Linking firmware does not execute board policy or
# protocol tests, hence the library tests.
#
# NOTE: FSTART_EXTRA_RUSTFLAGS must NOT be set here. Resolved builds fail
# closed on unrecorded ambient inputs (see tools/fbuild/src/selection.rs).

set -euo pipefail

if [[ $# -lt 2 ]]; then
	echo "Usage: $0 SIZE_DIR BOARD..." >&2
	exit 2
fi
size_dir="$1"
shift
mkdir -p "$size_dir"

# Fold each board's output and annotate failures when GitHub reads the log.
github() {
	[[ -z ${GITHUB_ACTIONS:-} ]] || echo "$1"
}

failed=()
for board; do
	github "::group::$board"
	start=$SECONDS
	if cargo fbuild build --board "$board" --release \
		--size-report "$size_dir/stage-sizes-$board.jsonl" &&
		cargo fbuild test-library --board "$board"; then
		result=PASS
	else
		result=FAIL
		failed+=("$board")
	fi
	github '::endgroup::'
	printf '%s %s (%ds)\n' "$result" "$board" "$((SECONDS - start))"
	[[ $result == PASS ]] || github "::error title=$board::build or library tests failed"
done

printf '\n%d of %d boards passed\n' "$(($# - ${#failed[@]}))" "$#"
if [[ ${#failed[@]} -gt 0 ]]; then
	printf 'failed: %s\n' "${failed[*]}"
	exit 1
fi
