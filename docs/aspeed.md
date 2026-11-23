# ASPEED BMC SoCs

fstart replaces the boot loader of ASPEED BMC SoCs: it brings up the chip
and DRAM, verifies the firmware image and boots a payload (a TamaGo program
such as cairn, or another bare ELF). It does not run the BMC itself.

| SoC | Main core | Boot | DRAM | Status |
| --- | --- | --- | --- | --- |
| AST2400 | ARM926EJ-S | XIP from SPI CE0 | DDR2/DDR3 at `0x4000_0000` | stages run on QEMU `palmetto-bmc`; DRAM must already be up |
| AST1250 | ARM926EJ-S | as AST2400 | as AST2400 | as AST2400 (OCP Leopard) |
| AST2500 | ARM1176JZF-S | XIP from SPI CE0 | DDR3/DDR4 at `0x8000_0000` | boots on hardware (Gigabyte MJ11-EC1) |
| AST2600 | 2x Cortex-A7, Cortex-M3 | ROM, then SPI | DDR4 at `0x8000_0000` | named in the board facts only; plans fail |

The AST1250 is the AST2400 die without host graphics, PCIe, the ColdFire
coprocessor and two of the four system UARTs. Both read the same silicon
revision in SCU7C, so the board names the SoC (`AspeedSoc::Ast1250`).

## Where the code lives

| Layer | Crate | Contents |
| --- | --- | --- |
| Registers | `aspeed-pac` ([kyanitecomputer/aspeed-data](https://github.com/kyanitecomputer/aspeed-data)) | Generated from YAML; one chip view per core (`ast2400-arm`, `ast2500-arm`, ...) |
| Drivers | `aspeed-hal` ([kyanitecomputer/aspeed-rs](https://github.com/kyanitecomputer/aspeed-rs)) | Blocking drivers: FMC fast read calibration, timer delay, AST2500 reset, DRAM init and MAC clocks |
| Platform | `crates/platform-aspeed` | Board facts, address maps, SPI image head, build plan, stages |
| Boards | `boards/gigabyte/mj11-ec1-bmc`, `boards/ocp/leopard-bmc` | Facts and board choices; each stage `main` is one call |

Both external crates are pinned by git revision in the workspace
`Cargo.toml`, and `aspeed-hal` pins the same `aspeed-data` revision, so
there is one PAC. `aspeed-hal` builds for one SoC at a time.

In `platform-aspeed`:

- `facts.rs`: `AspeedSoc`, `AspeedEmulator`, `BoardFacts`.
- `layout.rs`: the flash windows shared by all SoCs, and one `Layout` per
  SoC (DRAM, SRAM, main stage, handoff page, timestamp table).
- `head.rs`: the vector table at flash offset 0 and the AST2500 strap block.
- `host.rs`: the build plan and the QEMU run recipe.
- `stage.rs`: what every XIP SoC shares: console, boot timestamps, the CE0
  read setting, memory test, loading the main stage and the whole main stage.
- `ast2400.rs`, `ast2500.rs`: each SoC's bootblock and the `main_stage` entry.

## Boot flow

The image is padded to the CE0 flash. Its first 16 MiB, what 3-byte SPI
addressing reaches after reset, hold the bootblock followed by the FFS.

1. **Bootblock**, executing CE0 in place from address 0, with data, heap and
   stack in SRAM:
   - stops the watchdogs that fire on their own: the alternate-boot WDT2,
     and on the AST2500 the SPI address mode WDT3;
   - makes CE0 read-only, unlocks the SCU and starts the console;
   - holds the ColdFire coprocessor in reset (see below);
   - AST2500: takes one full SoC reset on a cold boot, which clears state
     that the fast power-on reset leaves behind;
   - switches CE0 to the dual output fast read at the fastest SPI clock up to
     the board's limit that a DMA checksum against a normal read confirms;
   - AST2500: sets up the MAC clocks and pins, then trains DRAM;
   - AST2400/AST1250: takes over DRAM that is already up (SCU40\[6\]);
   - optionally tests DRAM, authenticates the FFS root, loads the main stage
     into DRAM and writes the handoff page below it.
2. **Main stage**, 32 MiB into DRAM: caches DRAM and the flash window
   (write-through), mounts and verifies the FFS, loads the payload ELF and
   jumps to its entry with the MMU and caches off.

The FFS is read through CE0's decode window at `0x2000_0000`; address 0 is
not usable as a buffer address.

### Address maps

| | AST2400/AST1250 | AST2500 | AST2600 (planned) |
| --- | --- | --- | --- |
| SRAM | `0x1E72_0000`, 32 KiB | `0x1E72_0000`, 36 KiB | `0x1000_0000`, 89 KiB |
| DRAM | `0x4000_0000` | `0x8000_0000` | `0x8000_0000` |
| Handoff page | `0x41FF_F000` | `0x81FF_F000` | `0x81FF_F000` |
| Main stage (2 MiB) | `0x4200_0000` | `0x8200_0000` | `0x8200_0000` |
| Timestamp table | last 1 KiB of SRAM | last 1 KiB of SRAM | last 1 KiB of SRAM |

## Board choices

A board crate gives the facts (`AspeedSoc`, CE0 flash size, QEMU machine)
and passes its choices to the SoC's `bootblock`:

- **Flash clock limit** (`FLASH_MAX_HZ`): the highest SPI clock to try. The
  MJ11 uses 66 MHz, since HCLK/2 never calibrates there.
- **MAC timing** (AST2500, `MacConfig`): per MAC the RGMII transmit and
  receive clock delays, the transmit clock duty, the RMII reference clock
  output and whether MDIO goes to its pins. Which MAC is RGMII or RMII comes
  from the straps.
- **DRAM** (AST2500, `DramConfig`): speed, organisation, DDR4 ODT and CK
  drive, ECC range.
- **Memory test** (`MEMTEST`): an own-address test over usable DRAM.
- **Boot timestamps** (`TIMESTAMPS`, in the board library so both stages
  agree): timer 4 counts the milestones at 1 MHz and the main stage prints
  a report before the payload jump. Off by default.
- **Strap block** (AST2500, `spi_head!(STRAP_WRITES)`): register writes the
  SoC replays from flash before the CPU starts, when the SPI flash strap
  auto fetch (SCU70\[31\]) is set. The MJ11 uses it for a 32 MiB VGA
  carve-out. AST2400 boards use `spi_head!()`, the vectors alone.

## What fstart sets up and what the payload owns

fstart sets up what is fixed by the board and needed before any OS driver,
and leaves engines with a runtime to the payload.

| | fstart | Payload |
| --- | --- | --- |
| MACs | Reference clocks, MAC bus clock, RGMII/RMII delays and duty, MDIO pins; MACs left in reset | MAC reset and clock gating, driver, PHY / NC-SI |
| MAC addresses | none | Reads them from wherever the board keeps them (FRU EEPROM or flash) |
| ColdFire coprocessor (AST2400, AST2500) | Held in reset | Segment map, firmware image, release from reset, IPC |
| Cortex-M3 (AST2600), SSP/TSP (AST2700) | Held in reset (planned) | Firmware image and release |
| Timers 1-2 | unused | free |
| Timers 3-4 | Delays and boot timestamps, stopped before the jump | free after the jump |

The coprocessor is off after power-on, but a reset of the ARM alone (a
watchdog in ARM-only mode, or one whose reset mask leaves the coprocessor
out) leaves it running, possibly from DRAM that the bootblock retrains and
the stages overwrite, so the bootblock stops it on every boot. Its firmware
depends on the payload's IPC and the work the payload hands it, so the
payload loads it. If a board ever needs the coprocessor running before the
OS (fan control during a long boot, say), its image could become a signed
FFS file that the main stage loads; that is not implemented.

## Building and running

```sh
cargo fbuild build --board gigabyte-mj11-ec1-bmc --release
cargo fbuild qemu --board gigabyte-mj11-ec1-bmc --release --payload elf --kernel app.elf
cargo fbuild qemu --board ocp-leopard-bmc --release --payload elf --kernel app.elf
cargo fbuild assemble --board gigabyte-mj11-ec1-bmc --release --payload elf --kernel app.elf
```

The payload is a bare ELF linked into DRAM (`--payload elf`), or `halt`.
QEMU runs the image in place from CE0 (`ast2500-evb` or `palmetto-bmc`,
`execute-in-place=on`, with a flash model of the board's CE0 size). Both
machines start with DRAM already initialised, so the bootblock skips
training. The assembled image is the whole CE0 flash; write it off-board
with an SPI programmer.

## AST2600

The AST2600 starts in its boot ROM, which under secure boot verifies the
SPI image and may copy it into SRAM before running it; its Cortex-A7
cores are ARMv7-A. It needs its own
head and stage flow rather than the XIP stages above. The board facts,
address map and QEMU machine (`ast2600-evb`) are named so boards can be
declared, but plans for it fail until those stages exist.
