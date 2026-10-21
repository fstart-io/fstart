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
  per IP version, gated by the additive `naples`/`rome`/`milan` features.
- `crates/platform-sp3`: the boot flow, memory map, MP, SMM, PCI root
  bridges, ACPI and the host build plan / flash image. A board enables
  every generation it supports; the reset image identifies the CPU and runs
  the flow with that generation's silicon steps (`flow::naples` or
  `flow::sp3`). A CPU of a generation the build does not carry halts at
  POST 13 before the console; one the board does not list halts after
  logging.
- `boards/tyan/s8030`: board `tyan-s8030`, Rome and Milan in one build.

## One build per board

A board lists its generations (`Sp3BoardFacts::GENERATIONS`) and enables
their platform features; its `Board` hooks (policy, GPIO, detect pads,
DXIO engines) take the running generation. One build produces one reset
image and one FFS, and the assembler puts both into every half of the
vendor ROM whose EFS generation the board supports. Halves of other
generations keep their vendor bytes (with a warning).

## Flash image

The PSP, ABL, SMU, PMU and DXIO firmware and the APCB stay vendor blobs.
Each composed 16 MiB half keeps the vendor prefix (EFS, PSP and BIOS
directories) byte-identical, directories included. The PSP boot ROM picks
the half by the EFS generation word: Naples must be below Rome, Rome below
Milan; the assembler checks that order on a 32 MiB image.

Every vendor `0x62` (reset image) slot ends at flash `0x1000000` and DRAM
`0x76000000`; only its start differs (Naples KNPP-D32 `0xde0000`,
Naples S8026 and Rome `0xdb0000`, Milan v4.04 `0xce0000`, Milan v4.00
`0xd00000`). The reset image is linked once at the smallest donor slot,
KNPP-D32's `[0x75de0000, 0x76000000)` (`0x220000` bytes, a linker
`ASSERT` keeps it there), and placed at the end of each half's vendor
slot. The PSP copies the whole slot, so the image lands at its link
address in every half; the slot bytes in front of it are erased. A slot
smaller than the image, ending elsewhere or reaching into the early region
below `0x75c00000` is refused.

```text
[0, prefix end)               vendor bytes, byte-identical
[prefix end, 0x600000)        erased
[0x600000, FFS end)           fstart FFS, identical in every half
[FFS end, 0xde0000)           erased (front of a larger vendor slot)
[0xde0000, 16 MiB)            reset image, identical in every half
```

The FFS end is the lowest slot start among the composed halves, read from
the vendor ROM at assembly (`0xce0000` for the S8030 v4.04 dual image), so
the FFS and the FFS locator patched into the reset image are valid in
every half. The image links the upper bound `[0x600000, 0xde0000)` as its
firmware window and reads only what the locator describes.

Put the vendor ROM (S8030 v4.04, 32 MiB) at
`boards/tyan/s8030/vendor/8030V404.ROM` and build:

```sh
cargo fbuild build --board tyan-s8030              # compile only
cargo fbuild assemble --board tyan-s8030           # both halves
cargo fbuild assemble --board tyan-s8030 --payload linux \
    --kernel bzImage --linux-bootargs "console=ttyS0,115200"
cargo fbuild assemble --board tyan-s8030 --payload uefi
cargo fbuild assemble --board tyan-s8030 --image-config /path/to/other.rom
```

`assemble` writes `<board>.rom` next to the FFS in
`target/fstart-build/<board>/<profile>/<hash>/image/`: the vendor ROM's
size (32 MiB for the S8030) with every supported half composed.
`--image-config` paths are workspace-relative or absolute.

The assembler also refuses a vendor prefix reaching into the FFS
(`0x600000`, above every donor's prefix: Naples `0x273000`, Rome
`0x54b000`, Milan `0x586000`) and an FFS larger than its window. The FFS
holds the payload and a copy of the reset image, whose trust policy and
FFS locator are patched into the reset image before it goes into the
halves. Composition lives in `fstart_image_build::sp3_flash`.

The reverse engineering behind each block is in the `sp3` analysis
workspace (`reports/*_SP3.md`, `reports/NATIVE_PORT_PLAN.md`).
