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
image and one FFS; the assembler puts them into the vendor ROM so that
every supported generation boots them. The lower half must be of a
supported generation (it holds the FFS); an upper half of another
generation keeps its vendor bytes (with a warning) and still boots its
vendor firmware.

## Flash image

The PSP, ABL, SMU, PMU and DXIO firmware and the APCB stay vendor blobs:
every supported half keeps its PSP/BIOS directories and blobs in place,
`[0, prefix)` of the half (Naples `0x273000`, Rome `0x54b000`, Milan
`0x586000`), byte-identical apart from the pointer fields named below.
Composition lives in `fstart_image_build::sp3_flash` and is re-read with
the runtime directory parser and the boot ROM's EFS rule before the image
is written. References are AMD #55758 rev 1.13 (PSP BIOS Architecture
Design Guide, NDA; facts only) and the sp3 workspace's
`reports/FIRMWARE_IMAGE_SP3.md` §1.2a.

### Reset image

The reset image is linked once, at the smallest vendor `0x62` geometry
(Naples KNPP-D32: `0x220000` bytes at `[0x75de0000, 0x76000000)`, a linker
`ASSERT` keeps it there). The assembler rewrites **every** `0x62` entry
(L1 and L2, every composed generation) to that image: source flash
`0xde0000`, size `0x220000`, destination `0x75de0000`, reset + copy,
uncompressed (§4.1.5 Table 16). The vendor `0x62` slot bytes are erased.
The flash copy ends at the top of its half, so the ROM2 window at
`0xff000000` also shows it at `0xffde0000..0x1_0000_0000`.

### Layout: one vendor EFS per half, one FFS

The vendor's "Two 16M in 32MB" layout (§4.1.7): each composed half keeps
its vendor EFS and directories; only its `0x62` entries change, to a copy
of the reset image at that half's `0xde0000` (half-relative offsets; MMIO
form for Naples). Boot order stays the vendor's: a boot ROM takes the
first half whose EFS word it accepts, so Naples must be below Rome and
Rome below Milan (FIRMWARE_IMAGE §1.2).

```text
[0, prefix_lo)            lower vendor prefix (EFS, directories, blobs)
[ffs, 0xde0000)           FFS, ffs = max(0x600000, end of the prefix)
[0xde0000, 16M)           reset image (lower half's 0x62)
[16M, 16M + prefix_hi)    upper vendor prefix
[16M + 0xde0000, 32M)     reset image copy (upper half's 0x62)
```

The FFS exists once, in the lower half. An upper half without an EFS, or
of a generation the board does not support, keeps its vendor bytes. No
`0x55aa55aa` may sit at another boot ROM search offset (`0xfa0000`,
`0xf20000`, `0xe20000`, `0xc20000`, `0x820000`) of a composed half; the
FFS and reset image are checked.

Not implemented: a single multi-generation EFS (§4.1.7 "Single 32M in
32MB", `+0x24` with several generation bits clear) with a `2PSP` combo
directory in lookup mode 0 (§4.1.4.2) and the upper trees rebased to 32 MiB
offsets. It would need one reset image copy only, but no server boot ROM
has been seen to accept it; it is in the history (`uptxmqxy`).

### Finding the FFS and the directories at run time

The assembler patches a flash anchor into the reset image
(`fch::rom3::FlashAnchor`: magic `SP3FLASH`, part size, FFS flash offset
and size); the FFS locator inside the reset image stays FFS-relative as
for every fstart image. The FFS is read through ROM2 when ROM2 shows the
lower half (Naples, Rome) and through **ROM3** by absolute flash offset
otherwise (Milan booted from the upper half).

| Window | Address | Maps | Source |
|---|---|---|---|
| ROM2 | `[0xff000000, 4 GiB)` | 16 MiB page `SPIx5C[1:0]` (SPI_ROM_page, XOR of address bits 24/25) | PPR 55570 §9.1.13, §9.2.9.2; AMD #55758 §4.1.7 |
| ROM3 | `[0xfd_0000_0000, +64 MiB)` | the whole flash | PPR 55570 §9.1.13 ("LPC/SPI ROM_3"); illumos `zen/physaddrs.h` |

ROM3 lies in the 12 GiB hole `[0xfd_0000_0000, 1 TiB)` that DRAM and the
DF planner's MMIO pools skip; the reset image's 1 GiB identity map covers
it and reads it uncached. At step `0x14` the flow reads `SPIx5C` and uses
ROM2 for everything in the page it shows: the booted half's EFS always is
(the boot ROM selects it with the same XOR), and so is the FFS unless
Milan booted from the upper half. Only then does it probe ROM3 against the
EFS ROM2 shows (`FlashMap::probe`: linear, paged by the same XOR, or
absent); anything neither window shows halts. ROM3 is never read on a
Naples or Rome boot.

Open hardware questions (no public source; the run time checks and fails
closed), all on the ROM3 path, so on Milan boots only: whether ROM3
decodes after the PSP/ABL hand over on SSP/GN and needs an enable (the
Rome/Milan APCB has a `FchRom3BaseHigh` token, `0x7fc` on the S8030, `0`
in Oxide's Milan configurations, meaning unknown) and whether the DF
routes the hole to the FCH; whether ROM3 applies the `SPI_ROM_page` XOR;
whether the controller issues 4-byte addresses for host reads above
16 MiB; and what reading an undecoded ROM3 address does.

### Building

Put the vendor ROM (S8030 v4.04, 32 MiB) at
`boards/tyan/s8030/vendor/8030V404.ROM`, or name another one (e.g. the
target board's own flash backup) with `--image-config`, and build:

```sh
cargo fbuild build --board tyan-s8030              # compile only
cargo fbuild assemble --board tyan-s8030
cargo fbuild assemble --board tyan-s8030 --payload linux \
    --kernel bzImage --linux-bootargs "console=ttyS0,115200"
cargo fbuild assemble --board tyan-s8030 --payload uefi
cargo fbuild assemble --board tyan-s8030 --image-config /path/to/other.rom
```

`assemble` writes `<board>.rom` next to the FFS in `target/fstart-build/<board>/<profile>/<hash>/image/`,
the vendor ROM's size (32 MiB for the S8030). `--image-config` paths are
workspace-relative or absolute. For the S8030 v4.04 and for the test
bench's own Rome-only build the FFS window is `[0x600000, 0xde0000)`
(`0x7e0000` bytes).

The assembler refuses: a supported lower half missing, the same generation
in both halves, a vendor prefix reaching the reset image, an FFS larger
than its window, unknown address modes, stray EFS signatures, and any directory checksum or `0x62` entry
that does not verify after composition. The FFS holds the payload and a
copy of the reset image, whose trust policy, FFS locator and flash anchor
are patched into the PSP-copied reset image.

The reverse engineering behind each block is in the `sp3` analysis
workspace (`reports/*_SP3.md`, `reports/NATIVE_PORT_PLAN.md`).
