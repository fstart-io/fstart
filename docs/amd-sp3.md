# AMD SP3 (Naples, Rome, Milan)

Native support for the SP3 socket: EPYC 7001 (Naples), 7002 (Rome) and
7003 (Milan), one or two sockets, no AGESA. One firmware build per board
serves every generation the board lists. Hardware status lives in the
board READMEs.

- Boards: `boards/tyan/s8030` (`tyan-s8030`), `boards/amd/ethanolx` (`amd-ethanolx`)
- Platform: `crates/platform-sp3`
- Silicon drivers: `crates/driver-amd/src/sp3`
- Flash composer: `crates/image-build/src/sp3_flash.rs`

## Generations

| Generation | Silicon | CPUID signatures accepted | DF | SMU |
|---|---|---|---|---|
| Naples (EPYC 7001) | ZP | `0x00800F00`, `0x00800F10`–`0x00800F12` | DF2 | V9 |
| Rome (EPYC 7002) | SSP | `0x00830F00`, `0x00830F10` | DF3 | V11 |
| Milan (EPYC 7003) | GN | `0x00A00F00`, `0x00A00F10`–`0x00A00F12` | DF3 | V11 |

Source: `crates/driver-amd/src/cpu.rs` (`Revision::from_signature`),
`crates/driver-amd/src/sp3/mod.rs` (block table).

- Any other stepping, model or family halts at POST 13 before the console.
- The package check is CPUID `8000_0001` EBX[31:28] = 4 (`soc::PACKAGE_SP3`).
- `soc::Generation::MAX_SOCKETS` is 2.

## Code layout

| Path | Contents |
|---|---|
| `crates/driver-amd/src/sp3/` | `soc` (identify, per-generation constants), `topology`, `df`, `nbio`, `dxio`, `ccx`, `fch` |
| `crates/driver-amd/src/` | `apob`, `psp`, `smn`, `smu` (used by the SP3 flow) |
| `crates/platform-sp3/src/` | `board` (Board trait), `layout` (memory map, `Sp3BoardFacts`), `flow` (boot steps), `mp`, `memory`, `smm`, `smbios`, `acpi`, `pci`, `einj`, `host` (build plan) |
| `crates/platform-sp3/asm/reset.S`, `linker/reset.ld.in` | reset entry and the reset image linker script |
| `crates/platform-sp3/build.rs` | checks the SMM image fits TSEG |
| `crates/image-build/src/sp3_flash.rs` | vendor ROM composition and verification |
| `crates/amdfw-format` | flash anchor, EFS, BIOS/PSP directory readers |
| `crates/driver-spd` | DDR4 SPD decoding, independent of the EEPROM transport |
| `crates/acpi/src/smbios.rs` | SMBIOS writer shared with Intel |

Platform features (`crates/platform-sp3/Cargo.toml`):

| Feature | Effect |
|---|---|
| `naples`, `rome`, `milan` | additive: compile that generation's flow steps (`fstart-driver-amd/<gen>`) |
| `acpi` | build ACPI and SMBIOS |
| `bundle-reset` | reset-image runtime (used by fbuild's reset unit) |
| `payload-linux`, `payload-uefi` | payload backend (`fstart_payload` cfg) |
| `smm`, `bundle-smm` | FCH SMI handler, for the SMM image unit |
| `host` | build-plan code, never in firmware |

## Board contract

A board implements two traits: `layout::Sp3BoardFacts` (plan data) and
`board::Board` (hooks). Examples are in `boards/tyan/s8030/src/platform.rs`
and `boards/amd/ethanolx/src/lib.rs`.

`Sp3BoardFacts`:

| Item | Meaning |
|---|---|
| `GENERATIONS` | generations the board supports; each must have its platform feature enabled |
| `VENDOR_IMAGE` | workspace-relative vendor ROM (the board's flash or a dual image) |
| `MAX_CPUS` | logical CPU ceiling; sizes AP stacks and SMM entry stubs |
| `FLASH_MIRRORS` | offsets past the 16 MiB boot bank where the flash export mirrors it (default none) |

`Board` hooks: `NAME`, `CONSOLE` (16550), `policy(gen)`, `early_init(fch)`,
`detect_pads(gen)`, `gpio(gen)`, `dxio_engines(gen, levels, fch, timer)`,
optional `BMC_SX_NOTIFY`, and with `acpi`: `smbios_identity()`,
`acpi_lpc_aml()`, `acpi_board(aml)`.

Generation-dependent hooks receive the running CPU's generation. The flow
calls them only with one the board lists.

| Board | Id | GENERATIONS | VENDOR_IMAGE | MAX_CPUS | FLASH_MIRRORS |
|---|---|---|---|---|---|
| `boards/tyan/s8030` | `tyan-s8030` | Rome, Milan | `boards/tyan/s8030/vendor/8030V404.ROM` | 128 | none |
| `boards/amd/ethanolx` | `amd-ethanolx` | Milan | `boards/amd/ethanolx/vendor/RXM100CB.bin` | 256 | `0x0300_0000` |

Vendor ROMs are not in the repository (see each `vendor/README.md`).
`tyan-s8030-pcie3-x4x4x4x4` is a variant of `tyan-s8030` (feature
`pcie3-x4x4x4x4`).

## Boot flow

`flow::run` is one fixed sequence. `flow::identify()` picks the silicon
steps: `flow::naples` for Naples, `flow::sp3` for Rome and Milan.

| POST | Step (`flow/mod.rs`) |
|---|---|
| 10 | ECAM |
| 11 | FCH reset tasks, board early init |
| 12 | console |
| 13 | CPU identity (generation must be in `GENERATIONS`) |
| 14 | flash view and APOB |
| 15 | memory map, TSEG placement, e820, MTRRs |
| 20, 21 | DF topology and maps; DF init |
| 30, 31, 32 | NBIO early; SMU; FCH secondaries and USB |
| 38 | board detect pads, board GPIO, board DXIO engines, DXIO training |
| 40 | FCH env |
| 48 | boot media (mount FFS) |
| 50 | MP and CCX |
| 58 | SMM |
| 60 | PCI enumeration |
| 70 | ACPI and SMBIOS |
| 80 | ReadyToBoot (FCH late, DF lock, PSP LockDfReg, BootDone) |
| 90 | payload |

- A failed step logs its name and halts; the last POST code shows where.
- A CPU of a generation the build does not carry halts at POST 13 with no
  console. A generation the board does not list halts at 13 after logging.
- S3 resume is not implemented: the flow logs it and cold-boots.
- Without an SMM image in the build, step 58 logs it and SMM stays unlocked.

## Reset image memory map

Defined in `crates/platform-sp3/src/layout.rs`.

| Constant | Range | Use |
|---|---|---|
| `RESET` | `[0x75de_0000, 0x7600_0000)`, flash `0xde_0000`, 0x22_0000 bytes | reset image (BIOS entry `0x62`) |
| `CACHE_WINDOW` | `[0x7500_0000, 0x7600_0000)` | write-back MTRR window |
| `EARLY_REGION` | `[0x7500_0000, 0x75c0_0000)` | firmware-owned DRAM below the image |
| `PAGE_TABLES` | 256 PDPTs of 1 GiB pages, from `0x7500_0000` | identity map (`reset.S`) |
| `DATA` / `HEAP` / `STACK` / `STORE` | `0x7520_0000` / `0x7560_0000` / `0x75a0_0000` / `0x75b0_0000` | BSS and AP stacks / heap / BSP stack / store |
| `ROM3` | `[0xfd_0000_0000, +64 MiB)` | whole flash, above 4 GiB |
| `FFS_WINDOW` | `[0xfd_0060_0000, 0xfd_00de_0000)` | FFS upper bound: lower-half flash `[0x60_0000, 0xde_0000)` |

The reset image is linked once at the smallest vendor geometry and every
`0x62` entry of every composed generation points at it. The linker script
asserts it fits (`reset.ld.in`). The 16-bit entry sits in the last 64 KiB
and the reset vector in the last 16 bytes of the image.

## Flash image

### PSP firmware from files

With `--donor <vendor ROM>` or `--blobs <directory>` fstart packs the PSP
firmware itself: one two-level partition per generation the board lists,
from `crates/platform-sp3/Cargo.toml` (`[package.metadata.fstart.amd.naples]`,
`.rome`, `.milan`) and the board's APCB tables, each in its own 16 MiB half in
boot ROM order, `[0x20000, 0x600000)`, with its `0x62` entries naming the
reset image. The composer then lays out the FFS and the reset image as for a
vendor ROM. The partition format and tables are in
[amd-image-format.md](amd-image-format.md).

```sh
cargo fbuild assemble --board tyan-s8030 --release \
  --donor S8030GM2NE_BIOS_V4.04/8030V404.ROM --payload uefi
cargo fbuild assemble --board amd-ethanolx --release --donor RXM100CB.bin
```

Microcode for the FFS comes from the tables' `[[...microcode]]` files.
Packed SP3 images are not yet tested on hardware.

### Layout

Without `--donor`/`--blobs`, `sp3_flash` composes the build into the vendor
ROM. Each composed half keeps its vendor EFS, directories and blobs in place
(`[0, prefix)`), and only its `0x62` entries change.

```text
[0, prefix_lo)            lower vendor prefix (EFS, directories, blobs)
[ffs, 0xde0000)           FFS, ffs = max(0x60_0000, prefix end)
[0xde0000, 16M)           reset image (lower half's 0x62)
[16M, 16M + prefix_hi)    upper vendor prefix (kept if unsupported)
[16M + 0xde0000, 32M)     reset image copy (upper half's 0x62)
```

- The FFS exists once, in the lower half.
- Naples boots only from the lower half. Rome and Milan search both halves.
  `check_dual` requires Naples below Rome and Rome below Milan.
- An upper half without an EFS, or of a generation the board does not
  support, is kept byte-identical.
- The composed image is re-read with `fstart-amdfw-format` and checked
  before it is written.

### Runtime access

The reset image finds its FFS from a flash anchor the composer patches
in (`fstart_amdfw_format::anchor::FlashAnchor`, magic `SP3FLASH`, version 1,
fields: flash size, FFS offset, FFS size). The FFS locator and trust policy
are patched the same way.

| Window | Address | Maps |
|---|---|---|
| ROM2 | `[0xff00_0000, 4 GiB)` | one 16 MiB page, selected by `SPIx5C` `SPI_ROM_PAGE` |
| ROM3 | `[0xfd_0000_0000, +64 MiB)` | the flash, mapping not confirmed by public docs |

`flow::flash()` reads the EFS and the FFS through ROM2 when they lie in the
page ROM2 shows. It probes ROM3 (`fch::rom3::FlashMap::probe`) only for
data outside that page. The probe compares ROM3 with ROM2 at the EFS, for
linear and XOR-paged mapping. Anything neither window shows halts.

ROM3's enable, base, XOR and address width are not in the public PPRs. The
code checks them at run time and fails closed.

### Mirrored banks

A board whose flash export is larger than the 16 MiB boot bank and mirrors
it lists the mirror offsets in `FLASH_MIRRORS`. Given the full readout, the
composer writes the composed bank at 0 and at each mirror and keeps the
rest of the readout. `fbuild assemble` also writes the bank alone as
`<board>.bank.rom`. A bare 16 MiB bank composes to itself.

### Refusals

The composer fails the build on:

- no supported lower half, or a lower half of an unsupported generation
- the same generation in both halves
- a vendor prefix reaching the reset image, or an FFS larger than its window
- a `0x62` entry or directory that does not verify after composition
- a boot order that does not match the composed halves
- an EFS signature at another boot-ROM search offset (`efs::PROBES`)

Not implemented: a single multi-generation EFS with a combo directory
(AMD #55758 §4.1.7 "Single 32M in 32MB"). `sp3_flash.rs` states this.

## SMM

The SMM handler is a separate unit (`smm`) of the board's build, embedded
into the reset unit through `FSTART_SMM_IMAGE`.

- Unit: board library with features `smm`, `bundle-smm` and the board's
  generations; `fstart_stage_env = "smm"`, entry `x86_64`, log level off,
  release profile only. Position-independent, with `PIE` and `-Zplt=yes`.
- `image-build` rejects load-base-dependent relocations, GOT references and
  panic symbols.
- One entry stub per CPU (`MAX_CPUS`) and an 8 KiB stack each (`SMM_STACK_SIZE`).
- `build.rs` lays the image out for `FSTART_MP_MAX_CPUS` threads in an
  8 MiB TSEG (`smm::TSEG_SIZE`). An image that does not fit fails the build.

Installation at step 58 (`flow::install_smm`, arch `cpu::amd::smm::AmdSmm`):

1. `smm::place_tseg`: highest 8 MiB-aligned window below TOM and 4 GiB,
   outside `CACHE_WINDOW` and APOB. It is reserved in e820.
2. FCH SMIs off (`SmiEnB`). TValid/AValid cleared on the BSP. TSEG zeroed,
   the image copied in, and the result flushed.
3. Every thread (APs first, BSP last) writes SMM_ADDR, SMM_MASK, its
   SMM_BASE and sets HWCR.SmmLock, then reads the values back. After the
   lock, writes are ignored.
4. FCH sources are routed to SMI: SW SMI, SLP_TYP and power button, then
   EOS. Rome also traps CF9. Each thread takes one self-SMI through its
   new entry.
5. The PSP's fake-SMI source (SmiStat1 bit 1) is routed. SW SMI `0xD0`
   makes the handler send SmmInfo (C2P `0x02`) with its own SMRAM buffers.
6. Milan, with SMM installed: ReadyToBoot raises SW SMI `0xD2`, and the
   handler sends LockFCHReg.

Handler (`smm::Sp3SmmHandler`) services: ACPI enable/disable, Sx entry,
power button, the NBIO USB SMI, Rome's CF9 trap, SW SMI `0xD0`/`0xD2`,
and the PSP P2C mailbox. P2C requests are answered; SPI NV, RPMC and ARS
services are not implemented and return "unsupported". After each P2C
answer the handler clears SmiTrig0[25], so the PSP's SMI source does not
stay asserted.

## SMBIOS and ACPI

- Built at step 70 by `platform-sp3/src/smbios.rs` over the shared writer
  (`fstart_acpi::smbios`). The entry point is an SMBIOS 3.x 64-bit
  `_SM3_` structure. Type 17 uses the 3.3 layout.
- Types: 0 BIOS, 1 system, 3 enclosure, 4 processor, 7 cache, 16 memory
  array, 17 memory device, 19 mapped address, 32 system boot, 127 end.
  Type 2 and Type 11 appear only when the identity supplies them. No Type 20.
- CPUID is read on one enabled thread per package before the APs are parked
  for the payload. Cache totals come from the enabled APIC IDs.
- DIMM positions come from APOB (`mem::smbios_info`). Each present DIMM is
  decoded from its SPD with `fstart_driver_spd::ddr4::inventory`. Configured
  speed is 2 × MEMCLK, not the SPD maximum.
- A present DIMM whose SPD is unavailable or unsupported is reported as
  present with unknown capacity.
- If the APOB memory seeds are missing, or positions are invalid or
  duplicated, memory inventory is omitted with a warning.
- Maximum DIMM size per position: 128 GiB (Naples), 256 GiB (Rome, Milan).
- The entry point reaches UEFI payloads through `X86UefiPayloadContext::smbios`.
  The Linux handoff context has no SMBIOS accessor.
- Board identity comes from `Board::smbios_identity()`. CPU and DIMM
  inventory is never hard-coded.
- ACPI tables are built at step 70 under `platform-sp3/src/acpi/`.

## Build and assemble

Run from the repository root (`cargo fbuild` is the alias
`run --package fbuild --` in `.cargo/config.toml`). Flags match
`tools/fbuild/src/main.rs` and `tools/fbuild/src/payload.rs`.

```sh
# Build the units without composing a ROM (add --stage <unit> for one unit)
cargo fbuild build --board tyan-s8030 --release

# Compose the vendor ROM; writes <board>.rom in the artifact directory
cargo fbuild assemble --board tyan-s8030 --release

# Payloads: halt (default), linux, uefi
cargo fbuild assemble --board tyan-s8030 --release --payload linux \
    --kernel bzImage --linux-bootargs "console=ttyS0,115200"
cargo fbuild assemble --board tyan-s8030 --release --payload uefi

# Another vendor ROM (workspace-relative or absolute path)
cargo fbuild assemble --board tyan-s8030 --release --image-config /path/to/flash.bin

# Variant
cargo fbuild assemble --board tyan-s8030-pcie3-x4x4x4x4 --release

# Ethanol-X: full readout or lower 16 MiB bank as the vendor ROM
cargo fbuild assemble --board amd-ethanolx --release
```

- SP3 accepts `halt`, `linux` and `uefi`. Other `--payload` values are rejected.
  `--linux-bootargs` and `--kernel` need `--payload linux`.
- Output: `target/fstart-build/<board>/<release|debug>/<hash>/`.
  `<board>.rom` is the vendor ROM's size (32 MiB for a dual image, the full
  readout for a mirrored board). With mirrors, `<board>.bank.rom` is also written.
- The SMM unit is built release-only, whatever `--release` says.

## Related docs

- [amd-sp3-ras.md](amd-sp3-ras.md): native SMM MCA observer
- [amd-sp3-einj.md](amd-sp3-einj.md): ACPI EINJ support
- `boards/tyan/s8030/README.md`, `boards/amd/ethanolx/README.md`: board status, bifurcation, SKUs
