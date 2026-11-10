# AMD Onyx

AMD SP5 reference board, one socket, EPYC 9004 (Genoa), twelve DDR5
channels, AST2600 BMC. Status: boots a RAM-only Linux on all 160 CPUs of the
test specimen with native ACPI (IOMMU, NBIO IOAPICs, C-states, the PSP's
EINJ), SMBIOS, SMM for the PSP and ACPI S5. A build without `--payload
linux` launches CrabEFI, which binds the xHCIs and NVMe drives itself and
boots what it finds after a five-second countdown.

Built on `fstart-platform-rs` (feature `genoa`); the flow and the per-SoC
facts are in `crates/platform-rs`.

```sh
cargo fbuild build --board amd-onyx --release
# The image, packed from AMD's public Genoa blobs (coreboot's
# 3rdparty/amd_blobs, `genoa/PSP/...`) and the board's APCBs (`onyx/...`)
# in one blob directory:
cargo fbuild assemble --board amd-onyx --release --payload linux \
    --blobs /path/to/blobs \
    --compiler-input FSTART_RS_LINUX=/path/to/bzImage
# Or extract them from a firmware image of the board:
cargo fbuild blobs --board amd-onyx --from flash-backup.bin --out /path/to/blobs
```

Genoa's Linux payload requires microcode: the SoC table's patch
(`Typex66_0_3_0_UCodePatch.bin`, equivalence ID `A110`), or one flat Genoa
patch given as `--compiler-input FSTART_RS_MICROCODE=...`; the
PSP-installed level is not enough for the OS. The console is the SoC's
UART1 (`0xfedca000`, the BMC's SOL).

## Flash image

fstart packs the whole 32 MiB image: the EFS at `0x20000`, the two-level
PSP and BIOS directories with AMD's blobs and the board's APCBs, and the
8 MiB reset image (BIOS directory `0x62`) at `[0x800000, 0x1000000)`, which
the PSP copies to `[0x0a000000, 0x0a800000)`. The PSP writes the APOB to
`0x07001000`. A firmware image of the board only donates the APCBs (and
the blobs, if they aren't taken from AMD's).

## Recovery

Keep two matching full-chip reads of the original flash and an off-board
copy before writing. The BMC reaches the host flash as the `pnor` MTD under
controller `1e631000.spi`; mux pins O0/O1 (offsets 112/113 of
`gpiochip816`) select it, `(O1:O0) = 01` meaning the local BIOS SPI. Find
the device by name, never by a fixed `mtd` number (`mtd0..5` are the BMC's
own firmware), restore both pins to input and require two exact full-chip
readbacks before booting.

## Layout

| File | Content |
|---|---|
| `src/wiring.rs` | lane map from the public reference design, MPIO requests, USB policy, PERST release |
| `src/smm.rs` | the PSP's SMM handler (SMM unit only) |
| `src/lib.rs` | the `Board` facts: flash, console, required and retained PCIe endpoints, VGA, SMBIOS |

## Not done yet

- HMAT and `_CPC`; Genoa defaults are taken from openSIL without a capture
  of the vendor firmware's registers.
- The xHCI on IOS0 reports a host controller error after its halt before a
  Linux handoff; Linux resets it.
- Uncorrectable and PCIe error injection are untested.
