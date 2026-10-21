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

### Unified layout (default)

One EFS in the lower half serves every generation: §4.1.7 "Single 32M in
32MB SPIROM (1 X 32)" (directory addresses are offsets in the 32 MiB
image) with a `2PSP` combo directory at EFS `+0x14` (§4.1.4.2, Tables
10–12).

```text
[0, prefix_lo)            lower vendor prefix; EFS at 0x20000 rewritten
[prefix_lo, +0x1000)      2PSP (4 KiB aligned after the prefix)
[ffs, 0xde0000)           FFS, ffs = max(0x600000, end of prefix and 2PSP)
[0xde0000, 16M)           reset image (the only copy)
[16M, 16M + prefix_hi)    upper vendor prefix, trees rebased (+16 MiB)
[16M + prefix_hi, 32M)    erased (TODO: a second FFS region)
```

- **EFS** (lower, `0x20000`): `+0x14` → `2PSP`; `+0x18` Naples, `+0x20`
  Rome, `+0x28` Milan `$BHD` (no `2BHD`); `+0x24` = AND of the composed
  generations' words (Naples `ffffffff`, Rome `fffffffe`, Milan
  `fffffffc`; §4.1.7.1/§4.1.7.2: Rome needs bit 0 clear, Milan bit 1, and
  several bits may be clear). Every other byte (SPI/eSPI fields, xHCI
  pointer) stays the lower vendor's.
- **`2PSP`**: lookup mode 0 (the boot ROM tries each entry and takes the
  directory whose AMD root key hash matches; the three generations' root
  keys differ), `id_sel` 0, IDs from Table 12 (Naples `0xBC090000`, Rome
  `0xBC0B0000`, Milan `0xBC0B0D00`, used by fstart's own reader). The
  Naples entry is in MMIO form (`0xFF000000 | offset`), the others plain
  offsets, as on the AM4 precedent (B450 Pro4).
- **Upper trees** (every directory reachable from the upper EFS, through
  any entry pointing at a directory cookie): every location field gains
  `0x1000000` (address modes 0/1; mode 2 is directory-relative and stays;
  mode 3 and a non-zero directory base address are refused), directory
  address mode 0 becomes 1 (32 MiB offsets, §4.1.7), checksums are
  recomputed. Value entries (PSP `0x0b` soft fuse, BIOS `0x61` APOB) are
  skipped. MMIO pointers (Naples, gen1) only reach the half ROM2 shows and
  are refused in the upper half.
- **Upper EFS**: its signature is erased, so no boot ROM (e.g. Milan with
  `FUSE_EFS_32M_TOP_16M_FIRST`, §4.1.7.2 Table 22) finds the old 2x16 EFS
  whose trees moved. No `0x55aa55aa` may sit at another search offset
  (`0xfa0000`, `0xf20000`, `0xe20000`, `0xc20000`, `0x820000`) of either
  half; the FFS and reset image are checked.

A 16 MiB vendor ROM, or one with an unsupported upper half, keeps the
lower vendor EFS (single generation, no combo).

### Dual layout (fallback)

The vendor's "Two 16M in 32MB" (§4.1.7): each half keeps its vendor EFS
and directories; only the `0x62` entries change, to a reset image copy at
that half's `0xde0000` (half-relative offsets; MMIO form for Naples). The
FFS still exists once, in the lower half. Boot order is the vendor's: a
boot ROM takes the first half whose EFS word it accepts, so Naples must be
below Rome and Rome below Milan (FIRMWARE_IMAGE §1.2). Use it if a boot ROM
refuses the unified EFS on first boot:

```sh
cargo fbuild assemble --board tyan-s8030 --release --sp3-layout dual   # -> tyan-s8030-dual.rom
```

### Finding the FFS and the directories at run time

The FFS is read through the FCH **ROM3** window by absolute flash offset,
so it does not matter which half ROM2 shows. The assembler patches a flash
anchor into the reset image (`fch::rom3::FlashAnchor`: magic `SP3FLASH`,
part size, FFS flash offset and size); the FFS locator inside the reset
image stays FFS-relative as for every fstart image.

| Window | Address | Maps | Source |
|---|---|---|---|
| ROM2 | `[0xff000000, 4 GiB)` | 16 MiB page `SPIx5C[1:0]` (SPI_ROM_page, XOR of address bits 24/25) | PPR 55570 §9.1.13, §9.2.9.2; AMD #55758 §4.1.7 |
| ROM3 | `[0xfd_0000_0000, +64 MiB)` | the whole flash | PPR 55570 §9.1.13 ("LPC/SPI ROM_3"); illumos `zen/physaddrs.h` |

ROM3 lies in the 12 GiB hole `[0xfd_0000_0000, 1 TiB)` that DRAM and the
DF planner's MMIO pools skip; the reset image's 1 GiB identity map covers
it and reads it uncached. At step `0x14` the flow reads `SPIx5C`, probes
ROM3 against the EFS ROM2 shows (`FlashMap::probe`: linear, paged by the
same XOR, or absent), then takes the EFS the generation's boot ROM takes
(lower half first; Naples lower only) and parses the directories from the
start of that half: in the unified layout that view spans the whole part,
so Milan's directories in the upper half resolve. Without a usable ROM3
only the half ROM2 shows is readable and anything outside it halts.

Open hardware questions (no public source; the run time checks and fails
closed): whether ROM3 decodes after the PSP/ABL hand over on SSP/GN and
needs an enable (the Rome/Milan APCB has a `FchRom3BaseHigh` token, `0x7fc`
on the S8030, `0` in Oxide's Milan configurations, meaning unknown) and
whether the DF routes the hole to the FCH; whether ROM3 applies the
`SPI_ROM_page` XOR; whether the controller issues 4-byte addresses for
host reads above 16 MiB (the PSP reads the upper half itself, so the part
is in a mode that reaches it); what reading an undecoded ROM3 address
does; whether the Rome and Naples boot ROMs honour a `2PSP` with lookup
mode 0 next to the other generations' entries (documented for Family 17h
Models 00h on, seen on AM4, not on SP3); and whether every boot ROM
accepts EFS pointers and directory entries above 16 MiB (documented for
"Single 32M", not observed). The dual layout avoids the last two.

### Building

Put the vendor ROM (S8030 v4.04, 32 MiB) at
`boards/tyan/s8030/vendor/8030V404.ROM` and build:

```sh
cargo fbuild build --board tyan-s8030              # compile only
cargo fbuild assemble --board tyan-s8030           # unified layout
cargo fbuild assemble --board tyan-s8030 --sp3-layout dual
cargo fbuild assemble --board tyan-s8030 --payload linux \
    --kernel bzImage --linux-bootargs "console=ttyS0,115200"
cargo fbuild assemble --board tyan-s8030 --payload uefi
cargo fbuild assemble --board tyan-s8030 --image-config /path/to/other.rom
```

`assemble` writes `<board>.rom` (`<board>-dual.rom` for the dual layout)
next to the FFS in `target/fstart-build/<board>/<profile>/<hash>/image/`,
the vendor ROM's size (32 MiB for the S8030). `--image-config` paths are
workspace-relative or absolute. For the S8030 v4.04 both layouts give an
FFS window of `[0x600000, 0xde0000)` (`0x7e0000` bytes); the unified layout
puts the `2PSP` at `0x54b000` and leaves the upper half free from
`0x1586000` (`0xa7a000` bytes).

The assembler refuses: a supported lower half missing, the same generation
in both halves (unified), a vendor prefix reaching the reset image, an FFS
larger than its window, upper-half MMIO pointers (unified), unknown address
modes, stray EFS signatures, and any directory checksum or `0x62` entry
that does not verify after composition. The FFS holds the payload and a
copy of the reset image, whose trust policy, FFS locator and flash anchor
are patched into the PSP-copied reset image.

The reverse engineering behind each block is in the `sp3` analysis
workspace (`reports/*_SP3.md`, `reports/NATIVE_PORT_PLAN.md`).
