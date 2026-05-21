#!/bin/bash
# Create a GPT disk image with one FAT32 ESP for CrabEFI boot-chain tests.
# Layout:
#   /EFI/BOOT/BOOTX64.EFI
#   /boot/grub/grub.cfg
#   /vmlinuz
#   /initramfs.cpio

set -euo pipefail

if [[ $# -ne 5 ]]; then
	echo "usage: $0 OUTPUT.img BOOTX64.EFI vmlinuz initramfs.cpio grub.cfg" >&2
	exit 2
fi

OUTPUT="$1"
GRUB_EFI="$2"
KERNEL="$3"
INITRAMFS="$4"
GRUB_CFG="$5"

for path in "$GRUB_EFI" "$KERNEL" "$INITRAMFS" "$GRUB_CFG"; do
	[[ -f "$path" ]] || {
		echo "missing input: $path" >&2
		exit 1
	}
done

DISK_SIZE=$((64 * 1024 * 1024))
SECTOR_SIZE=512
ESP_START_SECTOR=2048
TOTAL_SECTORS=$((DISK_SIZE / SECTOR_SIZE))
ESP_END_SECTOR=$((TOTAL_SECTORS - 34))
ESP_SECTORS=$((ESP_END_SECTOR - ESP_START_SECTOR + 1))
ESP_BYTES=$((ESP_SECTORS * SECTOR_SIZE))
OFFSET_BYTES=$((ESP_START_SECTOR * SECTOR_SIZE))

mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT" "$OUTPUT.fat"
truncate -s "$DISK_SIZE" "$OUTPUT"
sgdisk --clear \
	--new=1:${ESP_START_SECTOR}:${ESP_END_SECTOR} \
	--typecode=1:ef00 \
	--change-name=1:ESP \
	"$OUTPUT" >/dev/null

truncate -s "$ESP_BYTES" "$OUTPUT.fat"
mkfs.fat -F 32 -n ESP "$OUTPUT.fat" >/dev/null

dd if="$OUTPUT.fat" of="$OUTPUT" bs="$SECTOR_SIZE" seek="$ESP_START_SECTOR" conv=notrunc status=none
rm -f "$OUTPUT.fat"

DISK_WITH_OFFSET="${OUTPUT}@@${OFFSET_BYTES}"
mmd -i "$DISK_WITH_OFFSET" ::/EFI ::/EFI/BOOT ::/boot ::/boot/grub 2>/dev/null || true
mcopy -i "$DISK_WITH_OFFSET" "$GRUB_EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$DISK_WITH_OFFSET" "$GRUB_CFG" ::/boot/grub/grub.cfg
mcopy -i "$DISK_WITH_OFFSET" "$KERNEL" ::/vmlinuz
mcopy -i "$DISK_WITH_OFFSET" "$INITRAMFS" ::/initramfs.cpio

ls -lh "$OUTPUT"
