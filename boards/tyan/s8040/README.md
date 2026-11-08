# Tyan S8040GM2NE

AMD SP6, one socket, EPYC 8004 (Siena), six DDR5 channels (twelve DIMM
slots), AST2600 BMC. Status: boots Linux on all 128 threads of a 64-core part,
either as a ROM-resident payload or through CrabEFI from a USB stick (the
Arch Linux ISO, BLS or its own systemd-boot), with native ACPI (NUMA,
IOMMU, NBIO IOAPICs, the PSP's EINJ), SMBIOS and SMM for the PSP.

Built on `fstart-platform-rs` (feature `siena`); the flow and the per-SoC
facts are in `crates/platform-rs`.

```sh
cargo fbuild build --board tyan-s8040 --release
# CrabEFI (the default payload): finds and drives the boot controllers itself.
cargo fbuild assemble --board tyan-s8040 --release --donor /path/to/flash-backup.bin
# A ROM-resident Linux (bzImage with an embedded initramfs).
cargo fbuild assemble --board tyan-s8040 --release --payload linux \
    --donor /path/to/flash-backup.bin \
    --compiler-input FSTART_RS_LINUX=/path/to/bzImage \
    --linux-bootargs 'console=ttyS0,115200 earlycon=uart8250,io,0x3f8,115200n8'
```

`--donor` extracts the files the tables name into a blob directory
(`cargo fbuild blobs --board tyan-s8040 --from ... --out ...` does the same
for `--blobs`): Siena's PSP and BIOS blobs and microcode patch (`AA02`,
loaded on every thread) from the SoC table of `fstart-platform-rs`, and the
board's APCBs. `--compiler-input FSTART_RS_MICROCODE=...` overrides the
patch. `FSTART_RS_LINUX` must support the ACPI RSDP boot protocol; the
kernel stays inside the reset image.

## Flash image

fstart packs the whole 64 MiB image: the EFS at `0x20000`, the two-level
PSP and BIOS directories, and the 6 MiB reset image (BIOS directory `0x62`)
at `[0xa00000, 0x1000000)`, which the PSP copies to
`[0x75a00000, 0x76000000)`; it writes the APOB to `0x75900000`. The NV
areas (the PSP's SEV state, the APOB NV copy) start erased. Nothing else of
the donor image (its layout, directories or UEFI volumes) is kept.

## Layout

| File | Content |
|---|---|
| `src/mpio.rs` | lane map and connector choices from the OEM tables, the MPIO port requests |
| `src/gpio.rs` | OEM CPM GPIO table, MCIO connector-mode pads, PCIe PERST release |
| `src/usb.rs` | USB port wiring and PHY policy |
| `src/bmc.rs` | AST2600 Super I/O (COM1 console), IPMI KCS |
| `src/smm.rs` | the PSP's SMM handler (SMM unit only) |
| `src/lib.rs` | the `Board` facts: flash, DIMM channels, required endpoints, VGA, SMBIOS |

## Not done yet

- `_CPC` and HMAT; IOHC client VC routing; CCX platform-flag records.
- Uncorrectable and PCIe error injection are untested.
- A read of the first USB block during the ROM Linux snapshot has hung a
  few times; a retry boots.
