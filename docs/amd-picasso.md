# Native AMD Picasso / Google Vilboz

Native (no FSP) port of AMD Picasso and the Google Zork "Vilboz" boards.

## Code layout

- `crates/driver-amd`: the silicon, by IP block (see
  [amd-zeppelin.md](amd-zeppelin.md#code-layout)). The Picasso versions,
  gated by the `picasso` feature: `df::picasso`, `nbio::picasso` (with the
  late PCIe power management), `dxio::picasso`, `ccx::picasso`, `smu::v10`
  (with the policy table), the Sandstone FCH `fch::sandstone` (A-Link and
  clocks, legacy, LPC/SPI preinit, USB, pin functions), the ACP `acp` and
  the SoC itself, `soc::picasso`. eMMC and eSPI are common FCH controllers
  (`fch::emmc`, `fch::espi`).
- `crates/platform-amd/src/picasso`: Picasso on the shared noncar flow
  ([amd-noncar.md](amd-noncar.md)): its `noncar::Soc` hooks, the board
  `Config`, DRAM layout and its host policy. The reset entry
  (`asm/psp_reset.S`), linker script, memory map, MP and the rest of the
  flow are shared with Zeppelin.
- `boards/google/zork`: Dalboz baseboard wiring and Vilboz policy. Vilboz,
  Vilboz14 and Vilboz360 differ only through EC CBI (board version,
  FW_CONFIG), so they share one board.
- `crates/amdfw`: the PSP flash image format;
  `crates/image-build/src/amd`, `tools/amdfw`: PSP/BIOS directory packing
  (the `fstart-amdfw` tool) and APCB SPD injection (`fstart-apcb`).

## x86 starts in DRAM

The PSP trains DRAM, writes the APOB and copies the BIOS reset image
(directory entry 0x62) to DRAM before releasing the BSP at its last 16
bytes. There is no cache-as-RAM. The layout is `picasso::LAYOUT`:

| Use | Range |
| --- | --- |
| APOB (PSP) | `02001000..02011000` |
| PSP shared memory (verstage only) | `02011000..02019000` |
| Page tables | `02020000..02026000` |
| Data, BSS | `02030000..02090000` |
| Heap | `02090000..020d0000` |
| BSP stack | `020d0000..02100000` |
| Reset image (2 MiB) | `02100000..02300000` |
| Reset vector | `022ffff0` |

`02000000..02300000` and the AP bootstrap page at `0x8000` stay reserved
through OS handoff. `.data` is initialized from the image into the data
region, so the PSP-copied bytes are never modified (the PSP checks them on
S3 resume).

APs are released by the SMU (ThreadEn) and also enter the reset vector.
`psp_reset.S` sends every non-BSP CPU to the shared SIPI trampoline at
`0x8000`, which the architectural MP code installs before the release.

## Boot flow

The noncar flow runs, in order:

1. ECAM, then the FCH pre-init (`fch_pre_init`): ACPI MMIO decode, legacy
   decode, AOAC power, UART pads; then the NS16550 console.
2. CPU identification, the APOB memory map, TSEG and e820, the AMD MTRRs
   (the BSP runs cached from here).
3. TP1 (`silicon_init`): data fabric routes, NBIO early init, PSP BAR,
   BSP/MCA init, DRAM routes; logs the memory map. Then the early FCH:
   LPC/SPI aperture, eSPI link to the EC, SPI ROM
   mode and speed, SMBus, board early GPIO tables, ROM-sharing disable.
4. `Board::fch_reset_done`: Vilboz reads CBI
   from the EC and programs the RAM-stage GPIOs with its board-revision
   overrides.
5. Halts at the checkpoint: Picasso's TP1 ends with `Stop::Checkpoint` and
   its `State` is uninhabited, so no later phase runs.

Implemented and tested in `driver-amd`, but not yet called: the SMU AP
release and the Picasso CCX init (`ccx::picasso::thread`), which the
shared `noncar::mp::start` would run; DXIO/PCIe training, USB, eMMC, NBIO
late/CCX-done, SMU power policy.

Still missing: CPU topology policy, SMM (relocation, TSEG), PCI enumeration,
ACPI/SMBIOS tables, graphics, payload handoff, S3 and APOB NV (MRC cache).

## Building

```sh
cargo fbuild build --board google-vilboz --release
```

Picasso has no FFS yet, so its microcode stays in the BIOS directory: the
type 0x66 files of the `--image-config` JSON are packed there. Its flow stops
before MP init; once it has an FFS, they go into `cpu_microcode.bin` as on
Zeppelin.

This produces the reset image only. `fbuild assemble --image-config
picasso-image.json` packs it with the PSP blobs listed in the JSON into a
PSP/BIOS directory fragment at the EFS flash offset
(`google-vilboz-psp-fragment.bin`, with a `.json` receipt). It is not a complete
flash image: a flashable image also needs the remaining Chromebook flash
layout (FMAP, RO/RW sections, VPD, GBB, EC firmware), which must be taken
from the board's existing firmware.

`tools/amdfw/tests` holds byte-for-byte differential tests against a reference
packer. Known gaps: APCB backups are aliases of the primary
APCB instead of separate copies as Zork builds them.
