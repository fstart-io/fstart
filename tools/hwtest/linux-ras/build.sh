#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Build the RAM-only RAS test kernel: a tinyconfig Linux with the AMD
# platform, serial consoles, MCE/EDAC, APEI (HEST, BERT, EINJ, GHES),
# PCIe AER and /dev/mem, /dev/cpu/*/msr and debugfs, and an initramfs of
# busybox, amd-ras and the test init. No block, network or filesystem
# drivers beyond what the initramfs needs: it cannot touch a target disk.
#
# Usage: build.sh LINUX_SOURCE OUT_DIR
#   LINUX_SOURCE  a Linux source tree (7.1 tested), left untouched
#   OUT_DIR       receives bzImage, .config and SHA256SUMS
#
# Needs Nix (busybox-static, the musl compiler and the kernel build tools).
set -euo pipefail
src=$(realpath "$1")
out=$(realpath -m "$2")
here=$(dirname "$(realpath "$0")")
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

out_path() { nix build --no-link --print-out-paths "$1" | grep -v -- '-man$' | head -1; }
busybox=$(out_path nixpkgs#pkgsStatic.busybox)/bin/busybox
cc=$(out_path nixpkgs#pkgsStatic.stdenv.cc)/bin/x86_64-unknown-linux-musl-cc

root=$work/root
mkdir -p "$root"/{bin,dev,proc,sys,tmp}
cp "$busybox" "$root/bin/busybox"
for applet in sh mount cat grep wc ls sleep dmesg awk od dd tr sed head tail \
	poweroff reboot devmem; do
	ln -s busybox "$root/bin/$applet"
done
"$cc" -static -O2 -Wall -Wextra -o "$root/bin/amd-ras" "$here/../amd-ras.c"
"$root/bin/amd-ras" selftest
install -m 755 "$here/init" "$root/init"
printf 'nod /dev/console 0600 0 0 c 5 1\n' > "$work/console.list"

nix-shell -p flex bison bc pkg-config elfutils zlib zstd openssl perl python3 \
	--run "
set -euo pipefail
make -C '$src' O='$work/build' tinyconfig
cfg='$src/scripts/config --file $work/build/.config'
for x in \$(grep -v '^#' '$here/ras.config'); do \$cfg --enable \"\$x\"; done
for x in MODULES ATA SCSI BLK_DEV_NVME NVME_CORE USB_STORAGE USB_UAS DRM FB SOUND NET \
	HYPERVISOR_GUEST KVM_GUEST AMD_PSTATE RANDOMIZE_BASE DEBUG_INFO DEBUG_INFO_BTF ACPI_SLEEP \
	BLK_DEV STRICT_DEVMEM IO_STRICT_DEVMEM; do
	\$cfg --disable \"\$x\"
done
\$cfg --set-val NR_CPUS 512 --set-val LOG_BUF_SHIFT 21 --set-str INITRAMFS_SOURCE '$root $work/console.list' \
	--enable INITRAMFS_COMPRESSION_XZ --enable KERNEL_XZ --disable KERNEL_GZIP \
	--set-str LOCALVERSION -fstart-ras --disable LOCALVERSION_AUTO
make -C '$src' O='$work/build' olddefconfig
make -C '$src' O='$work/build' -j\$(nproc) bzImage
"
cp "$work/build/arch/x86/boot/bzImage" "$work/build/.config" "$out/"
for f in BLK_DEV ATA SCSI BLK_DEV_NVME USB_STORAGE NET; do
	if grep -q "^CONFIG_$f=y" "$out/.config"; then
		echo "CONFIG_$f is on: refusing a kernel that can reach a disk" >&2
		exit 1
	fi
done
(cd "$out" && sha256sum bzImage .config > SHA256SUMS)
cat "$out/SHA256SUMS"
