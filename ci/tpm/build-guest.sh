#!/usr/bin/env bash
# Build the Linux guest of the TPM boot tests:
#   OUT/initramfs.cpio  busybox init printing the TPM PCRs and the event log
#   OUT/bios.iso        GRUB (i386-pc) El Torito image for SeaBIOS
#   OUT/uefi.img        GPT disk with a GRUB EFI ESP for CrabEFI
#
#   ci/tpm/build-guest.sh OUT KERNEL BUSYBOX GRUB_EFI_MKSTANDALONE
#
# GRUB_EFI_MKSTANDALONE is grub-mkstandalone of an x86_64-efi GRUB. Also
# needs grub-mkrescue (i386-pc), xorriso, mtools, sgdisk, mkfs.fat, cpio.
set -euo pipefail

out="${1:?usage: $0 OUT KERNEL BUSYBOX GRUB_EFI_MKSTANDALONE}"
kernel="${2:?kernel bzImage}"
busybox="${3:?static busybox}"
mkstandalone="${4:?grub-mkstandalone (x86_64-efi)}"
here="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$out"
work="$(mktemp -d)"
trap 'chmod -R u+w "$work"; rm -rf "$work"' EXIT

root="$work/root"
mkdir -p "$root"/{bin,dev,proc,sys}
cp "$busybox" "$root/bin/busybox"
chmod u+w "$root/bin/busybox"
for applet in sh mount cat echo ls base64 dmesg grep poweroff sleep sed; do
	ln -s busybox "$root/bin/$applet"
done
cat >"$root/init" <<'INIT'
#!/bin/sh
# Kernels without CONFIG_DEVTMPFS_MOUNT start init without a console.
mount -t devtmpfs devtmpfs /dev 2>/dev/null
exec </dev/console >/dev/console 2>&1
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t securityfs securityfs /sys/kernel/security
t=/sys/class/tpm/tpm0
echo TPMTEST_BEGIN
echo "TPMTEST version $(cat $t/tpm_version_major 2>/dev/null)"
for bank in sha1 sha256 sha384 sha512; do
	[ -d $t/pcr-$bank ] || continue
	for i in 0 1 2 3 4 5 6 7 8 9; do
		echo "TPMTEST pcr $bank $i $(cat $t/pcr-$bank/$i)"
	done
done
[ -f $t/pcrs ] && sed 's/^/TPMTEST pcrs /' $t/pcrs
[ -f $t/device/pcrs ] && sed 's/^/TPMTEST pcrs /' $t/device/pcrs
echo TPMTEST_LOG_BEGIN
base64 /sys/kernel/security/tpm0/binary_bios_measurements
echo TPMTEST_LOG_END
dmesg | grep -i -E 'tpm|pnp|MSFT|AE_|acpi.*error' | sed 's/^/TPMTEST dmesg /'
echo TPMTEST_END
poweroff -f
INIT
chmod +x "$root/init"
(cd "$root" && find . | cpio -o -H newc --quiet) >"$out/initramfs.cpio"

cmdline="console=ttyS0,115200 panic=-1 quiet"
cat >"$work/grub.cfg" <<CFG
set timeout=0
insmod tpm
linux /vmlinuz $cmdline
initrd /initramfs.cpio
boot
CFG

iso="$work/iso"
mkdir -p "$iso/boot/grub"
cp "$kernel" "$iso/vmlinuz"
cp "$out/initramfs.cpio" "$iso/initramfs.cpio"
sed '/insmod tpm/d' "$work/grub.cfg" >"$iso/boot/grub/grub.cfg"
grub-mkrescue -o "$out/bios.iso" "$iso" >/dev/null 2>&1

# GRUB with its TPM module measures what it loads (PCR 8 and 9).
cat >"$work/embedded.cfg" <<'CFG'
search --no-floppy --file /vmlinuz --set=root
configfile /boot/grub/grub.cfg
CFG
"$mkstandalone" -O x86_64-efi -o "$work/BOOTX64.EFI" \
	--modules="part_gpt fat search configfile linux tpm" \
	"boot/grub/grub.cfg=$work/embedded.cfg"
bash "$here/../create-grub-linux-disk.sh" "$out/uefi.img" "$work/BOOTX64.EFI" \
	"$kernel" "$out/initramfs.cpio" "$work/grub.cfg" >/dev/null
echo "guest images in $out"
