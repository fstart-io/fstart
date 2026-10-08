#!/usr/bin/env bash
# Drop firmware plan directories that the current CI run did not build so the
# cached target/ directory does not accumulate stale plans.
#
# Usage: ci/prune-build-cache.sh STAMP_FILE
#
# Firmware units live in target/fstart-build/<board>/<profile>/<plan-hash>/.
# fbuild rewrites the selection records of every unit it builds, so a plan
# directory with nothing newer than STAMP_FILE belongs to an older plan.
# target/fstart-build/cargo is the Cargo target directory all units share;
# Cargo overwrites workspace crates in place there, so it is kept.
set -euo pipefail

stamp="${1:?usage: $0 STAMP_FILE}"
root=target/fstart-build

[[ -d $root ]] || exit 0

find "$root" -mindepth 3 -maxdepth 3 -type d -not -path "$root/cargo/*" -print0 |
	while IFS= read -r -d '' plan; do
		if [[ -z $(find "$plan" -newer "$stamp" -print -quit) ]]; then
			echo "pruning stale plan ${plan#"$root"/}"
			rm -rf "$plan"
		fi
	done
find "$root" -mindepth 1 -type d -empty -delete
