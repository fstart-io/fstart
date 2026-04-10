#!/usr/bin/env bash
# Build external payload binaries needed for QEMU boot tests:
#   - OpenSBI fw_dynamic.bin  (RISC-V firmware)
#   - TF-A bl31.bin           (AArch64 EL3 firmware)
#   - Linux kernels            (vmlinux, Image, zImage per arch)
#
# Usage: ci/build-payloads.sh <output-dir>
#
# Environment variables (with defaults):
#   OPENSBI_VERSION  — OpenSBI tag to build (default: 1.6)
#   TFA_VERSION      — TF-A tag to build    (default: 2.12.0)
#   LINUX_VERSION    — Linux tag to build    (default: 6.12)
#
# Cross-compiler prefixes are auto-detected from PATH.
# On Ubuntu:  riscv64-linux-gnu-, aarch64-linux-gnu-, arm-linux-gnueabihf-
# On NixOS:   riscv64-unknown-linux-gnu-, aarch64-unknown-linux-gnu-, armv7l-unknown-linux-gnueabihf-

set -euo pipefail

OUTPUT_DIR="${1:?Usage: $0 <output-dir>}"
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(dirname "$SCRIPT_DIR")"

OPENSBI_VERSION="${OPENSBI_VERSION:-1.6}"
TFA_VERSION="${TFA_VERSION:-2.12.0}"
LINUX_VERSION="${LINUX_VERSION:-6.12}"

NPROC="$(nproc)"

# ---------------------------------------------------------------------------
# Cross-compiler prefix detection
# ---------------------------------------------------------------------------
detect_prefix() {
  local arch="$1"
  local candidates
  case "$arch" in
    riscv64) candidates="riscv64-linux-gnu- riscv64-unknown-linux-gnu-" ;;
    aarch64) candidates="aarch64-linux-gnu- aarch64-unknown-linux-gnu-" ;;
    arm)     candidates="arm-linux-gnueabihf- armv7l-unknown-linux-gnueabihf-" ;;
    *)       echo "unknown arch: $arch" >&2; exit 1 ;;
  esac
  for p in $candidates; do
    if command -v "${p}gcc" &>/dev/null; then
      echo "$p"
      return
    fi
  done
  echo "ERROR: no $arch cross-compiler found on PATH" >&2
  exit 1
}

RISCV64_CROSS="$(detect_prefix riscv64)"
AARCH64_CROSS="$(detect_prefix aarch64)"
ARM_CROSS="$(detect_prefix arm)"

echo "Cross-compiler prefixes:"
echo "  riscv64: ${RISCV64_CROSS}"
echo "  aarch64: ${AARCH64_CROSS}"
echo "  arm:     ${ARM_CROSS}"

# ---------------------------------------------------------------------------
# OpenSBI
# ---------------------------------------------------------------------------
echo ""
echo "=== OpenSBI v${OPENSBI_VERSION} ==="

OPENSBI_DIR="/tmp/opensbi-${OPENSBI_VERSION}"
if [ ! -d "$OPENSBI_DIR" ]; then
  git clone --depth 1 --branch "v${OPENSBI_VERSION}" \
    https://github.com/riscv-software-src/opensbi.git "$OPENSBI_DIR"
fi

make -C "$OPENSBI_DIR" \
  CROSS_COMPILE="$RISCV64_CROSS" \
  PLATFORM=generic \
  -j"$NPROC"

cp "$OPENSBI_DIR/build/platform/generic/firmware/fw_dynamic.bin" \
   "$OUTPUT_DIR/fw_dynamic.bin"
echo "  -> $OUTPUT_DIR/fw_dynamic.bin"

# ---------------------------------------------------------------------------
# TF-A BL31 (QEMU virt platform)
# ---------------------------------------------------------------------------
echo ""
echo "=== TF-A v${TFA_VERSION} (PLAT=qemu) ==="

TFA_DIR="/tmp/arm-trusted-firmware-${TFA_VERSION}"
if [ ! -d "$TFA_DIR" ]; then
  git clone --depth 1 --branch "lts-v${TFA_VERSION}" \
    https://github.com/ARM-software/arm-trusted-firmware.git "$TFA_DIR"
fi

make -C "$TFA_DIR" \
  CROSS_COMPILE="$AARCH64_CROSS" \
  PLAT=qemu \
  QEMU_USE_GIC_DRIVER=QEMU_GICV3 \
  bl31 \
  -j"$NPROC"

cp "$TFA_DIR/build/qemu/release/bl31.bin" "$OUTPUT_DIR/bl31.bin"
echo "  -> $OUTPUT_DIR/bl31.bin"

# ---------------------------------------------------------------------------
# Linux kernel — builds three architectures from the same source tree
# ---------------------------------------------------------------------------
echo ""
echo "=== Linux v${LINUX_VERSION} ==="

LINUX_DIR="/tmp/linux-${LINUX_VERSION}"
if [ ! -d "$LINUX_DIR" ]; then
  # Use kernel.org tarball — smaller and faster than git clone
  TARBALL="/tmp/linux-${LINUX_VERSION}.tar.xz"
  if [ ! -f "$TARBALL" ]; then
    MAJOR="${LINUX_VERSION%%.*}"
    curl -fSL \
      "https://cdn.kernel.org/pub/linux/kernel/v${MAJOR}.x/linux-${LINUX_VERSION}.tar.xz" \
      -o "$TARBALL"
  fi
  tar xJf "$TARBALL" -C /tmp
fi

build_kernel() {
  local karch="$1"
  local cross="$2"
  local config_frag="$3"
  shift 3
  # remaining args: "src_path:dst_name" pairs

  echo ""
  echo "--- Linux ${karch} ---"

  make -C "$LINUX_DIR" ARCH="$karch" mrproper
  make -C "$LINUX_DIR" ARCH="$karch" tinyconfig
  cat "${WORKSPACE_DIR}/ci/${config_frag}" >> "${LINUX_DIR}/.config"
  make -C "$LINUX_DIR" ARCH="$karch" CROSS_COMPILE="$cross" olddefconfig
  make -C "$LINUX_DIR" ARCH="$karch" CROSS_COMPILE="$cross" -j"$NPROC"

  for mapping in "$@"; do
    local src="${mapping%%:*}"
    local dst="${mapping##*:}"
    cp "${LINUX_DIR}/${src}" "${OUTPUT_DIR}/${dst}"
    echo "  -> ${OUTPUT_DIR}/${dst}"
  done
}

# RISC-V 64 — vmlinux (ELF, for qemu-riscv64) + Image (flat, for sifive-unmatched)
build_kernel riscv "$RISCV64_CROSS" kernel-riscv64.config \
  "vmlinux:vmlinux-riscv64" \
  "arch/riscv/boot/Image:Image-riscv64"

# AArch64 — Image (flat binary)
build_kernel arm64 "$AARCH64_CROSS" kernel-aarch64.config \
  "arch/arm64/boot/Image:Image-aarch64"

# ARMv7 — zImage (compressed)
build_kernel arm "$ARM_CROSS" kernel-armv7.config \
  "arch/arm/boot/zImage:zImage-armv7"

# ---------------------------------------------------------------------------
echo ""
echo "=== All payloads built ==="
ls -lh "$OUTPUT_DIR"
