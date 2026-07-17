#!/usr/bin/env bash
# QEMU boot matrix. Run inside the CI/development shell; this script does not
# invoke nix develop itself.
#
# Usage: ci/qemu-boot-tests.sh [asset-dir] [--board BOARD] [--payload PAYLOAD]
#
# QEMU_BOOT_TIMEOUT sets the timeout for each boot (default: 120s). Filters may
# be repeated and are useful when diagnosing one matrix entry.

set -euo pipefail

ASSET_DIR="boot-assets/payloads"
if [[ $# -gt 0 && $1 != --* ]]; then
	ASSET_DIR="$1"
	shift
fi

# x86_64 GRUB/Linux disk assets (ci/build-boot-assets.sh + create-grub-linux-disk.sh).
# The uefi-disk entry is skipped when the disk image is absent.
X86_ASSET_DIR="${QEMU_BOOT_X86_ASSETS:-boot-assets/x86_64}"

BOOT_TIMEOUT="${QEMU_BOOT_TIMEOUT:-120s}"
LOG_DIR="${QEMU_BOOT_LOG_DIR:-target/qemu-boot-tests}"
board_filters=()
payload_filters=()

usage() {
	echo "Usage: $0 [asset-dir] [--board BOARD] [--payload PAYLOAD]" >&2
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--board)
		board_filters+=("${2:?--board requires a board}")
		shift 2
		;;
	--payload)
		payload_filters+=("${2:?--payload requires a payload}")
		shift 2
		;;
	-h|--help)
		usage
		exit 0
		;;
	*)
		usage
		exit 2
		;;
	esac
done

matches_filter() {
	local value="$1"
	shift
	[[ $# -eq 0 ]] && return 0
	local filter
	for filter; do
		[[ $value == "$filter" ]] && return 0
	done
	return 1
}

mkdir -p "$LOG_DIR"
failures=0
selected=0

run_boot() {
	local board="$1"
	local payload="$2"
	local marker="$3"
	shift 3

	matches_filter "$board" "${board_filters[@]}" || return 0
	matches_filter "$payload" "${payload_filters[@]}" || return 0
	selected=$((selected + 1))

	# The matrix label may carry a variant suffix (uefi-disk); fbuild only
	# sees the payload kind before the first dash.
	local payload_arg="${payload%%-*}"
	local log="$LOG_DIR/${board}-${payload}.log"
	local -a command=(
		timeout --kill-after=10s "$BOOT_TIMEOUT"
		cargo run -q -p fbuild -- run --board "$board" --release --payload "$payload_arg"
	)
	command+=("$@")

	set +e
	"${command[@]}" >"$log" 2>&1
	local status=$?
	set -e

	if grep -Fq "$marker" "$log" && { [[ $status -eq 0 || $status -eq 124 ]]; }; then
		printf 'PASS %-14s %-6s %s\n' "$board" "$payload" "$marker"
	else
		printf 'FAIL %-14s %-6s %s (exit %d; %s)\n' \
			"$board" "$payload" "$marker" "$status" "$log"
		tail -n 40 "$log" >&2
		failures=$((failures + 1))
	fi
}

run_boot qemu-q35 halt 'ramstage: ready for payload'
run_boot qemu-q35 uefi 'Boot manager finished'
# Full boot chain: fstart -> CrabEFI -> GRUB (ESP) -> Linux -> u-root init.
if [[ -f "$X86_ASSET_DIR/disk.img" ]]; then
	run_boot qemu-q35 uefi-disk UROOT_BOOT_SUCCESS \
		--disk "$X86_ASSET_DIR/disk.img"
else
	printf 'SKIP %-14s %-6s %s (no %s)\n' qemu-q35 uefi-disk \
		UROOT_BOOT_SUCCESS "$X86_ASSET_DIR/disk.img"
fi

run_boot qemu-riscv64 halt 'ramstage: ready for payload'
run_boot qemu-riscv64 linux FSTART_CI_BOOT_SUCCESS \
	--kernel "$ASSET_DIR/Image-riscv64" --firmware "$ASSET_DIR/fw_dynamic.bin"
run_boot qemu-riscv64 uefi 'Boot manager finished' \
	--firmware "$ASSET_DIR/fw_dynamic.bin"

run_boot qemu-sifive-u halt 'sifive-u ramstage: ready for payload'
run_boot qemu-sifive-u linux FSTART_CI_BOOT_SUCCESS \
	--kernel "$ASSET_DIR/Image-riscv64" --firmware "$ASSET_DIR/fw_dynamic.bin"

run_boot qemu-aarch64 halt 'ramstage: ready for payload'
run_boot qemu-aarch64 linux FSTART_CI_BOOT_SUCCESS \
	--kernel "$ASSET_DIR/Image-aarch64" --firmware "$ASSET_DIR/bl31.bin"
run_boot qemu-aarch64 uefi 'Boot manager finished' \
	--firmware "$ASSET_DIR/bl31.bin"

run_boot qemu-armv7 halt 'ramstage: ready for payload'
run_boot qemu-armv7 linux FSTART_CI_BOOT_SUCCESS \
	--kernel "$ASSET_DIR/zImage-armv7"

# TF-A-first SBSA: fstart is BL33 in pflash1, entered NS from BL31.
if [[ -f "$ASSET_DIR/sbsa-secure.pflash" ]]; then
	run_boot qemu-sbsa halt 'qemu-sbsa ramstage: ready for payload' \
		--secure-firmware "$ASSET_DIR/sbsa-secure.pflash"
else
	printf 'SKIP %-14s %-6s %s (no %s)\n' qemu-sbsa halt \
		'qemu-sbsa ramstage: ready for payload' "$ASSET_DIR/sbsa-secure.pflash"
fi

# Orange Pi R1 (H2+) eGON SD boot on the QEMU orangepi-pc H3 machine.
run_boot orangepi-r1 halt 'h3 mainstage: 1024 MiB DRAM'
if [[ -f "$ASSET_DIR/sun8i-h2-plus-orangepi-r1.dtb" ]]; then
	cp "$ASSET_DIR/sun8i-h2-plus-orangepi-r1.dtb" boards/orangepi-r1/
	run_boot orangepi-r1 linux FSTART_CI_BOOT_SUCCESS \
		--kernel "$ASSET_DIR/zImage-armv7"
else
	printf 'SKIP %-14s %-6s %s (no %s)\n' orangepi-r1 linux \
		FSTART_CI_BOOT_SUCCESS "$ASSET_DIR/sun8i-h2-plus-orangepi-r1.dtb"
fi

if [[ $selected -eq 0 ]]; then
	echo 'No boot tests selected.' >&2
	exit 2
fi

printf '\nQEMU boot matrix: %d passed, %d failed\n' \
	"$((selected - failures))" "$failures"
(( failures == 0 ))
