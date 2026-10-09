#!/usr/bin/env bash
# Build SeaBIOS as an fstart coreboot payload and print the fbuild arguments.
#
#   payloads/seabios/build.sh [SEABIOS_SRC] [OUT_DIR]
#
# SEABIOS_SRC must carry the fstart-romfiles change (SeaBIOS reads its files,
# e.g. the VGA BIOS, from the coreboot table instead of CBFS).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "${1:-$here/../../../seabios-fstart}" && pwd)
out=$(mkdir -p "${2:-$here/../../target/seabios}" && cd "${2:-$here/../../target/seabios}" && pwd)

cp "$here/defconfig" "$out/.config"
make -C "$src" KCONFIG_CONFIG="$out/.config" OUT="$out/" olddefconfig >/dev/null
make -C "$src" KCONFIG_CONFIG="$out/.config" OUT="$out/" -j"$(nproc)" >/dev/null
# SeaBIOS mirrors its text screen (menus, boot messages) to this port.
python3 -c 'import struct,sys; sys.stdout.buffer.write(struct.pack("<Q", 0x3f8))' >"$out/sercon-port"

echo "--payload coreboot --kernel $out/bios.bin.elf" \
    "--payload-file vgaroms/seavgabios.bin=$out/vgabios.bin" \
    "--payload-file etc/sercon-port=$out/sercon-port"
