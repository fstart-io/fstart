# Native AMD Picasso / Google Vilboz

Native (no FSP) port of AMD Picasso and the Google Zork "Vilboz" boards.

## Code layout

- `crates/driver-amd`: register mechanisms (FCH, GPIO, SMN/DF, SMU, NBIO,
  DXIO, USB, eMMC, eSPI, PSP mailbox, microcode, APOB parsing). Gated by the
  `picasso` feature.
- `crates/platform-amd`: the Picasso flow, DRAM layout, reset entry
  (`asm/psp_reset.S`, shared with Zeppelin), linker script, MP policy and
  the host build plan.
- `boards/google/zork`: Dalboz baseboard wiring and Vilboz policy. Vilboz,
  Vilboz14 and Vilboz360 differ only through EC CBI (board version,
  FW_CONFIG), so they share one board.
- `crates/image-build/src/amd`, `tools/amdfw`: PSP/BIOS directory packing
  (`fstart-amdfw`) and APCB SPD injection (`fstart-apcb`).

## x86 starts in DRAM

The PSP trains DRAM, writes the APOB and copies the BIOS reset image
(directory entry 0x62) to DRAM before releasing the BSP at its last 16
bytes. There is no cache-as-RAM. The layout lives in
`crates/platform-amd/src/layout.rs` and `linker/picasso.ld`:

| Use | Range |
| --- | --- |
| APOB (PSP) | `02001000..02011000` |
| PSP shared memory (verstage only) | `02011000..02019000` |
| Microcode page, PSP mailbox page | `02019000..0201b000` |
| BSS | `02020000..020d0000` |
| Page tables | `020d0000..020d6000` |
| BSP stack | `020e0000..02100000` |
| Reset image (2 MiB) | `02100000..02300000` |
| Reset vector | `022ffff0` |

`02000000..02300000` and the AP bootstrap page at `0x8000` stay reserved
through OS handoff. The Picasso image has no initialized writable data, so
the PSP-copied bytes are never modified (the PSP checks them on S3 resume).

APs are released by the SMU (ThreadEn) and also enter the reset vector.
`psp_reset.S` sends every non-BSP CPU to the shared SIPI trampoline at
`0x8000`, which the architectural MP code installs before the release.

## Boot flow

`picasso::run` currently does, in order:

1. Console: ACPIMMIO decode, legacy decode, AOAC power, UART pads, NS16550.
2. MMCONF, APOB parse, `early::initialize`: AMD MTRRs (the BSP runs cached
   from here), data fabric routes, NBIO early init, PSP BAR, BSP/MCA init.
   Logs the memory map.
3. The early FCH: LPC/SPI aperture, eSPI link to the EC,
   SPI ROM mode and speed, SMBus, board early GPIO tables, ROM-sharing
   disable.
4. `BoardFacts::mainboard_init`: Vilboz reads CBI from the EC and programs
   the RAM-stage GPIOs with its board-revision overrides.
5. Halts.

Implemented and tested, but not yet called:

- MP: `cold::initialize_cpus` releases the APs through the SMU via
  `mp_init_with_launcher`; the Family 17h CPU driver (`cpu::Fam17hCpu`) loads
  the microcode and runs the CCX init on every CPU. APs replay the BSP's
  MTRRs and PAT and keep their own SYSCFG/TOP_MEM/TOM2 (`mp::sipi_msrs`).
- DXIO/PCIe training, USB, eMMC, NBIO late/CCX-done, SMU power policy, PSP
  mailbox.

Still missing: CPU topology policy, SMM (relocation, TSEG), PCI enumeration,
ACPI/SMBIOS tables, graphics, payload handoff, S3 and APOB NV (MRC cache).

## Building

```sh
cargo fbuild build --board google-vilboz --release
```

The CPUs load their microcode patch from the BIOS directory (type 0x66),
like Zeppelin's; `fstart-amdfw --microcode` packs AMD's patch files there.

This produces the reset image only. `fbuild assemble --image-config
picasso-image.json` packs it with the PSP blobs listed in the JSON into a
PSP/BIOS directory fragment at the EFS flash offset. It is not a complete
flash image: a flashable image also needs the remaining Chromebook flash
layout (FMAP, RO/RW sections, VPD, GBB, EC firmware), which must be taken
from the board's existing firmware.

`tools/amdfw/tests` holds byte-for-byte differential tests against a reference
packer. Known gaps: APCB backups are aliases of the primary
APCB instead of separate copies as Zork builds them.
