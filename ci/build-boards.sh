#!/usr/bin/env bash
# Build boards in release and run their host library tests, one after the
# other in the same Cargo target directory. A failing board never stops the
# ones after it; the script fails at the end if any did.
#
# Usage: ci/build-boards.sh SIZE_DIR BOARD...
#
# SIZE_DIR receives one stage-sizes-BOARD.jsonl per board (see
# `fbuild sizes`). Linking firmware does not execute board policy or
# protocol tests, hence the library tests. Boards CI cannot boot also
# assemble their payloads.
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

# CI cannot boot these boards, so at least assemble every payload their
# plans offer. Inputs are placeholders: this checks the plan's input
# inventory and image construction, not the payloads.
assemble_payloads() {
	case $1 in
	sifive-unmatched)
		local input dtb=boards/sifive/unmatched/unmatched.dtb status=0
		input=$(mktemp)
		printf 'fstart CI placeholder' >"$input"
		cp "$input" "$dtb"
		cargo fbuild assemble --board "$1" --release --payload halt &&
			cargo fbuild assemble --board "$1" --release --payload linux \
				--kernel "$input" --firmware "$input" &&
			cargo fbuild assemble --board "$1" --release --payload uefi \
				--firmware "$input" || status=$?
		rm -f "$input" "$dtb"
		return "$status"
		;;
	esac
}

failed=()
for board; do
	github "::group::$board"
	start=$SECONDS
	if cargo fbuild build --board "$board" --release \
		--size-report "$size_dir/stage-sizes-$board.jsonl" &&
		cargo fbuild test-library --board "$board" &&
		assemble_payloads "$board"; then
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
