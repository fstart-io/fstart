#!/bin/bash
# Build x86_64 boot-test assets for the fstart -> CrabEFI -> GRUB -> Linux
# QEMU Q35 CI path.  Output defaults to boot-assets/x86_64.

set -euo pipefail

ARCH="x86_64"
OUTPUT_DIR=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--arch)
		ARCH="$2"
		shift 2
		;;
	*)
		OUTPUT_DIR="$1"
		shift
		;;
	esac
done

if [[ "$ARCH" != "x86_64" ]]; then
	echo "Error: only --arch x86_64 is supported by this CI helper" >&2
	exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-${PROJECT_DIR}/boot-assets/${ARCH}}"
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

KERNEL_VERSION="6.13.7"
KERNEL_MAJOR="${KERNEL_VERSION%%.*}"
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz"
KERNEL_SHA256="3a39b62038b7ac2f43d26a1f84b4283e197804e1e817ad637e9a3d874c47801d"
# Pinned for reproducible CI boot assets.
UROOT_REF="c0ddf1088ef06310a88349fecda959d04291110b"

if [[ ! -f "$OUTPUT_DIR/vmlinuz" ]]; then
	echo "==> Building Linux ${KERNEL_VERSION} (x86_64, minimal EFI-stub config)"
	WORK="$(mktemp -d)"
	cleanup() {
		chmod -R u+w "$WORK" 2>/dev/null || true
		rm -rf "$WORK"
	}
	trap cleanup EXIT

	KERNEL_TARBALL="$WORK/linux-${KERNEL_VERSION}.tar.xz"
	curl --fail --show-error --location --silent "$KERNEL_URL" --output "$KERNEL_TARBALL"
	echo "${KERNEL_SHA256}  ${KERNEL_TARBALL}" | sha256sum -c -
	tar xJ -C "$WORK" -f "$KERNEL_TARBALL"
	KSRC="$WORK/linux-${KERNEL_VERSION}"
	make -C "$KSRC" -s ARCH=x86 tinyconfig
	KCFG="$KSRC/scripts/config"
	KCFG_ARGS=(--file "$KSRC/.config")
	"$KCFG" "${KCFG_ARGS[@]}" \
		--enable 64BIT --enable X86_64 --enable PRINTK --enable TTY \
		--enable SERIAL_8250 --enable SERIAL_8250_CONSOLE --enable EARLY_PRINTK \
		--enable EFI --enable EFI_STUB --enable ACPI --enable X86_X2APIC \
		--enable BLK_DEV_INITRD --enable TMPFS --enable DEVTMPFS \
		--enable DEVTMPFS_MOUNT --enable PROC_FS --enable SYSFS \
		--enable BINFMT_ELF --enable FUTEX --enable EVENTFD --enable EPOLL \
		--enable SIGNALFD --enable TIMERFD --enable VT --enable VT_CONSOLE \
		--enable UNIX98_PTYS --enable MULTIUSER --enable RELOCATABLE
	make -C "$KSRC" -s ARCH=x86 olddefconfig
	make -C "$KSRC" -s ARCH=x86 -j"$(nproc)" bzImage
	cp "$KSRC/arch/x86/boot/bzImage" "$OUTPUT_DIR/vmlinuz"
	cleanup
	trap - EXIT
fi

if [[ ! -f "$OUTPUT_DIR/initramfs.cpio" ]]; then
	echo "==> Building u-root initramfs (amd64)"
	UROOT_DIR="$(mktemp -d)"
	uroot_cleanup() {
		chmod -R u+w "$UROOT_DIR" 2>/dev/null || true
		rm -rf "$UROOT_DIR"
	}
	trap uroot_cleanup EXIT
	git clone --depth 1 -q https://github.com/u-root/u-root.git "$UROOT_DIR/src"
	(cd "$UROOT_DIR/src" && \
		actual_ref="$(git rev-parse HEAD)" && \
		[[ "$actual_ref" == "$UROOT_REF" ]] && \
		go build -o "$UROOT_DIR/bin/u-root" .)
	(cd "$UROOT_DIR/src" &&
		GOARCH=amd64 "$UROOT_DIR/bin/u-root" \
			-o "$OUTPUT_DIR/initramfs.cpio" \
			-format cpio \
			-defaultsh "" \
			-initcmd "" \
			./cmds/core/echo ./cmds/core/cat ./cmds/core/ls)
	uroot_cleanup
	trap - EXIT
fi

if [[ ! -f "$OUTPUT_DIR/grubx64.efi" ]]; then
	echo "==> Building GRUB EFI binary"
	GRUB_CFG="$(mktemp)"
	cat >"$GRUB_CFG" <<'GRUBCFG'
serial --unit=0 --speed=115200
terminal_input serial
terminal_output serial
set root=(memdisk)
linux /vmlinuz console=ttyS0,115200 earlycon=uart8250,io,0x3f8,115200n8 nokaslr panic=5 rdinit=/bbin/echo UROOT_BOOT_SUCCESS
initrd /initramfs.cpio
boot
GRUBCFG
	GRUB_MEMDISK="$(mktemp -d)"
	mkdir -p "$GRUB_MEMDISK/boot/grub"
	cp "$GRUB_CFG" "$GRUB_MEMDISK/boot/grub/grub.cfg"
	cp "$OUTPUT_DIR/vmlinuz" "$GRUB_MEMDISK/vmlinuz"
	cp "$OUTPUT_DIR/initramfs.cpio" "$GRUB_MEMDISK/initramfs.cpio"
	(
		cd "$GRUB_MEMDISK"
		tar cf memdisk.tar boot/grub/grub.cfg vmlinuz initramfs.cpio
	)
	grub-mkimage \
		--format=x86_64-efi \
		--output="$OUTPUT_DIR/grubx64.efi" \
		--prefix='(memdisk)/boot/grub' \
		--memdisk="$GRUB_MEMDISK/memdisk.tar" \
		--config="$GRUB_CFG" \
		linux memdisk tar terminal echo boot serial cpuid
	rm -rf "$GRUB_MEMDISK"
	rm -f "$GRUB_CFG"
fi

cat >"$OUTPUT_DIR/grub.cfg" <<'GRUBCFG'
serial --unit=0 --speed=115200
terminal_input serial
terminal_output serial
set timeout=0
set default=0
set root=(hd0,gpt1)
menuentry "Linux" {
    linux /vmlinuz console=ttyS0,115200 earlycon=uart8250,io,0x3f8,115200n8 nokaslr panic=5 rdinit=/bbin/echo UROOT_BOOT_SUCCESS
    initrd /initramfs.cpio
}
GRUBCFG

echo "==> Boot assets ready in $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR"
