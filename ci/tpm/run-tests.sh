#!/usr/bin/env bash
# Measured boot on QEMU q35: TPM 1.2 and TPM 2.0 (swtpm), with SeaBIOS (a
# coreboot payload) and CrabEFI, each booting the Linux guest of
# build-guest.sh. Every boot's event log must replay to the PCRs Linux reads.
#
#   ci/tpm/run-tests.sh GUEST_DIR SEABIOS_DIR
#
# GUEST_DIR holds bios.iso and uefi.img (build-guest.sh), SEABIOS_DIR the
# payloads/seabios/build.sh output. swtpm/swtpm_setup must be on PATH.
set -euo pipefail

guest="${1:?usage: $0 GUEST_DIR SEABIOS_DIR}"
seabios="${2:?usage: $0 GUEST_DIR SEABIOS_DIR}"
here="$(cd "$(dirname "$0")" && pwd)"
logs="${TPM_TEST_LOG_DIR:-target/tpm-tests}"
mkdir -p "$logs"
failures=0

run() {
	local name="$1" family="$2" banks="$3"
	shift 3
	local verify_args=(--banks "$banks" --expect EV_S_CRTM_VERSION@0
		--expect EV_S_CRTM_CONTENTS@0)
	while [[ $# -gt 0 && $1 != -- ]]; do
		verify_args+=(--expect "$1")
		shift
	done
	[[ ${1:-} == -- ]] || { echo 'missing -- before fbuild arguments' >&2; return 2; }
	shift
	local log="$logs/$name.log"
	timeout --kill-after=10s "${TPM_TEST_TIMEOUT:-300s}" \
		"$here/qemu-tpm.sh" "$family" --banks "$banks" -- \
		--board qemu-q35 --release "$@" >"$log" 2>&1 || true
	if python3 "$here/verify-log.py" "$log" "${verify_args[@]}" >"$log.verify"; then
		printf 'PASS %s\n' "$name"
	else
		printf 'FAIL %s (%s)\n' "$name" "$log.verify"
		tail -n 20 "$log.verify" >&2
		tail -n 40 "$log" >&2
		failures=$((failures + 1))
	fi
}

seabios_args=(--payload coreboot --kernel "$seabios/bios.bin.elf"
	--payload-file "vgaroms/seavgabios.bin=$seabios/vgabios.bin"
	--payload-file "etc/sercon-port=$seabios/sercon-port"
	--disk "$guest/bios.iso")
uefi_args=(--payload uefi --disk "$guest/uefi.img")

# POST_CODE also describes SMM; EV_IPL proves SeaBIOS continued measuring.
run tpm12-seabios 1.2 sha1 EV_POST_CODE@0 EV_IPL@4 -- "${seabios_args[@]}"
run tpm20-seabios 2.0 sha1,sha256 EV_POST_CODE2@0 EV_IPL@4 -- "${seabios_args[@]}"
run tpm12-crabefi 1.2 sha1 EV_EFI_BOOT_SERVICES_APPLICATION@4 -- "${uefi_args[@]}"
run tpm20-crabefi 2.0 sha1,sha256 EV_EFI_BOOT_SERVICES_APPLICATION@4 -- "${uefi_args[@]}"
run tpm20-crabefi-sha384 2.0 sha256,sha384 EV_EFI_BOOT_SERVICES_APPLICATION@4 -- "${uefi_args[@]}"

exit "$((failures != 0))"
