# Tyan S8040

AMD SP6, one socket, EPYC 8004 (Siena), six DDR5 channels of two slots,
AST2600 BMC on eSPI. Status: boots to the payload on all 128 threads of a
64-core part, with native ACPI (NUMA, IOMMU, the NBIO IOAPICs) and SMBIOS.

```sh
cargo fbuild build --board tyan-s8040 --release
# The image, packed from blobs extracted from a flash backup:
cargo fbuild assemble --board tyan-s8040 --release \
    --donor /path/to/flash-backup.bin
# A ROM Linux (bzImage with its initramfs):
cargo fbuild assemble --board tyan-s8040 --release --payload linux \
    --donor /path/to/flash-backup.bin --kernel /path/to/bzImage \
    --linux-bootargs 'console=ttyS0,115200'
```

`--donor` extracts the files the tables name (`cargo fbuild blobs` does
the same for `--blobs`): Siena's PSP and BIOS blobs and its microcode
patch from fstart-platform-amd's `siena` table, and the board's APCBs.

## Flash image

fstart packs the 16 MiB the boot ROM maps: the EFS at `0x20000`, the
two-level PSP and BIOS directories, the FFS at `[0x800000, 0xd00000)` and
the reset image (BIOS directory entry `0x62`) at `[0xd00000, 0x1000000)`,
which the PSP copies to `0x75d00000`; it writes the APOB to `0x75900000`.
The NV areas start erased, and the eSPI configuration of the EFS brings up
the link to the BMC before x86 runs. The rest of the 64 MiB part is not
used.

## Layout

| File | Content |
|---|---|
| `src/mpio.rs` | lane map and connector choices from the vendor tables |
| `src/gpio.rs` | the OEM CPM GPIO table and the MCIO connector-mode pads |
| `src/usb.rs` | USB port wiring and PHY policy |
| `src/bmc.rs` | AST2600 Super I/O, the COM1 console |
| `src/lib.rs` | the board's configuration of Siena: NBIO, cores, MPIO, DIMM channels, VGA |
