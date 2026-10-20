# AMD SP3: Naples, Rome, Milan

Native (no AGESA) support for the AMD SP3 socket in one and two socket
systems, and the Tyan S8030 board. Status: **in development, not booted**.

| Generation | Silicon | CPUID | Notes |
|---|---|---|---|
| Naples (EPYC 7001) | ZP, DF2 | Fam 17h M00h–0Fh | 4 dies per socket |
| Rome (EPYC 7002) | SSP, DF3 | Fam 17h M30h–3Fh | IOD + up to 8 CCDs |
| Milan (EPYC 7003) | GN, DF3 | Fam 19h M00h–0Fh | IOD + up to 8 CCDs |

## Code layout

- `crates/driver-zen`: silicon IP blocks (`smn`, `soc`, `topology`, `df`,
  `nbio`, `smu`, `dxio`, `ccx`, `fch`, `psp`, `apob`), one implementation
  per IP version, gated by the `naples`/`rome`/`milan` features.
- `crates/platform-sp3`: the per-generation boot flow, memory map, MP, SMM,
  PCI root bridges, ACPI and the host build plan / flash image.
- `boards/tyan/s8030`: variants `tyan-s8030-rome` (lower 16 MiB of the
  32 MiB flash) and `tyan-s8030-milan` (upper 16 MiB).

## Flash image

The PSP, ABL, SMU, PMU and DXIO firmware and the APCB stay vendor blobs.
Each 16 MiB half keeps the vendor prefix (EFS, PSP and BIOS directories)
and replaces the reset image (BIOS directory entry 0x62). The PSP boot ROM
picks the half by the EFS generation word: Rome must be the lower half,
Milan the upper.

```text
[0, prefix end)             vendor bytes, byte-identical
[prefix end, 0x600000)      erased
[0x600000, 0x62 offset)     fstart FFS (the runtime firmware window)
[0x62 offset, 16 MiB)       reset image (Rome 0xdb0000, Milan 0xce0000)
```

Put the vendor ROM (S8030 v4.04, 32 MiB) at
`boards/tyan/s8030/vendor/8030V404.ROM` and build each half:

```sh
cargo fbuild build --board tyan-s8030-rome            # compile only
cargo fbuild assemble --board tyan-s8030-rome         # lower half
cargo fbuild assemble --board tyan-s8030-milan        # upper half
cargo fbuild assemble --board tyan-s8030-rome --payload linux \
    --kernel bzImage --linux-bootargs "console=ttyS0,115200"
cargo fbuild assemble --board tyan-s8030-milan --payload uefi
cargo fbuild assemble --board tyan-s8030-rome --image-config /path/to/other.rom
```

`assemble` writes, next to the FFS in
`target/fstart-build/<board>/<profile>/<hash>/image/`, `<board>.rom` (the
16 MiB half) and, for a 32 MiB vendor ROM, `<board>-32m.rom`: the vendor
ROM with only this variant's half replaced, the other half byte-identical
and the boot order checked. Flashing `tyan-s8030-milan-32m.rom` keeps the
vendor AMI firmware for Rome CPUs (and vice versa); assembling with
`--image-config` pointing at the other variant's `-32m.rom` combines both
fstart halves. `--image-config` paths are workspace-relative or absolute.

The assembler refuses a vendor half of the wrong generation, a vendor
`0x62` geometry different from the build's (unless the board sets
`REWRITE_RESET_GEOMETRY`, which rewrites the L1/L2 entries and their
checksums), a prefix reaching into the FFS and an FFS larger than its
window. The FFS holds the payload and a copy of the reset image, whose
trust policy and FFS locator are patched into the `0x62` copy.
Composition lives in `fstart_image_build::sp3_flash`.

The reverse engineering behind each block is in the `sp3` analysis
workspace (`reports/*_SP3.md`, `reports/NATIVE_PORT_PLAN.md`).
