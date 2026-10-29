#!/usr/bin/env bash
# Run `cargo fbuild run` for a QEMU board with a swtpm-backed TPM.
#
#   ci/tpm/qemu-tpm.sh 1.2|2.0 [--banks sha1,sha256] -- <fbuild run arguments>
#
# swtpm and swtpm_setup must be on PATH. The TPM state is fresh for every run
# and removed afterwards; QEMU sees a tpm-tis device.
set -euo pipefail

family="${1:?usage: $0 1.2|2.0 [--banks LIST] -- FBUILD-ARGS}"
shift
banks="sha1,sha256"
while [[ $# -gt 0 && $1 != -- ]]; do
	case "$1" in
	--banks) banks="$2"; shift 2 ;;
	*) echo "unknown option $1" >&2; exit 2 ;;
	esac
done
[[ ${1:-} == -- ]] && shift

state="$(mktemp -d)"
trap 'kill "$(cat "$state/pid" 2>/dev/null)" 2>/dev/null || true; rm -rf "$state"' EXIT

case "$family" in
2.0)
	swtpm_setup --tpm2 --tpmstate "$state" --pcr-banks "$banks" \
		--createek --overwrite >"$state/setup.log" 2>&1
	tpm2=(--tpm2)
	;;
1.2)
	swtpm_setup --tpmstate "$state" --createek --overwrite >"$state/setup.log" 2>&1
	tpm2=()
	;;
*) echo "family must be 1.2 or 2.0" >&2; exit 2 ;;
esac

swtpm socket "${tpm2[@]}" --tpmstate dir="$state" \
	--ctrl type=unixio,path="$state/sock" --pid file="$state/pid" \
	--log file="$state/swtpm.log",level=1 --daemon

export FSTART_QEMU_EXTRA_ARGS="-chardev socket,id=chrtpm,path=$state/sock \
-tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0"
cargo fbuild run "$@"
