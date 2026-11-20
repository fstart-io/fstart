# fstart Architecture

This document is the top-level map of the repository: crate layering, how host
plans and firmware stages fit together, which patterns are canonical, and which
legacy areas are still being migrated.

## Layering

Every piece of code belongs to one area of responsibility:

| Layer | Owns | Lives in |
|---|---|---|
| Core & services | `no_std` types, barrier-aware MMIO/PIO, `Indexed`, service traits (`Console`, `Timer`, `BootMedia`, `FlashRegion`, `Measure`, `I2c`, `SpiBus`, `SmBus`), FFS/board schema | `crates/core` |
| Architecture | CPU startup, paging, MTRRs, LAPIC/IOAPIC, MP bring-up, barriers | `crates/arch` |
| Subsystems | PCI (`pci`), ACPI/SMBIOS (`acpi`, `acpi-macros`), FFS (`ffs`), FDT (`fdt`), stages/loading (`stage`, `load`, `boot`, `payload`, `smm`, `resume`, `store`, `options`, `tcg`, `timestamp`, `log`, `crypto`) | `crates/*` |
| Drivers | Hardware IP blocks and peripherals: registers, sequences, vendor tables. No board discovery, no SoC Cargo features | `crates/driver-*` |
| Platforms | SoC composition: which IP versions run in which order, memory/reset layout, ACPI/SMBIOS assembly, host build plan | `crates/platform-*` |
| Boards | Board policy: lanes, pads, GPIOs, on-board devices, stage entrypoints (`src/bin/*.rs`) | `boards/<vendor>/<board>` |
| Host tools | Image assembly, board builds, QEMU boot tests, CI checks (`fbuild`), register formatting (`ffmt`), AMD firmware (`amdfw`), release/web tools | `tools/*` |

## Host Plan vs Firmware Stages

Board crates are workspace members (`boards/*/*`), excluded from
`default-members` so plain `cargo test` exercises host libraries and tools.
`cargo fbuild` builds a board in two phases:

1. **Host plan** (`--features host`, host target): compiles the board's
   metadata/plan helper (`crates/platform-*/src/host*` + `crates/image-build`)
   to produce the exact stage list, memory/flash layout, linker scripts, and
   FFS image manifest.
2. **Firmware stages** (target triple + `-Zbuild-std=core,alloc`): compiles
   each stage binary (`src/bin/stage.rs`, or `bootblock.rs` / `postcar.rs` /
   `ramstage.rs`, plus optional `smm.rs`), with LTO in release builds and
   `--gc-sections`, then assembles and signs the final image.

Because stages run on tiny CAR/SRAM stacks (often 8 KiB), fbuild builds every
firmware crate with `opt-level = "s"`, in dev builds too
(`tools/fbuild/src/selection.rs`). Host tools and tests stay unoptimized.

## Canonical Patterns (Copy These)

- **IP blocks vs SoCs vs boards**: `crates/driver-amd` + `crates/platform-amd`
  (see `docs/amd-structure.md`). Drivers build every IP block with no SoC Cargo
  features; platforms compose SoCs with data/trait variants (e.g.
  `iod::Variant`, `rs::Variant`) and let LTO + `--gc-sections` drop unused
  flows.
- **MMIO & staged registers**:
  - Prefer an existing PAC/HAL when one exists (`aspeed-pac`/`aspeed-hal`,
    `rflasher-*`).
  - Otherwise define `register_bitfields!` + `register_structs!` using
    `fstart_core::mmio::{MmioReadWrite, MmioReadOnly, MmioWriteOnly}`, and
    use `fstart_core::mmio::{read8, write32, ...}` for one-off accesses.
    `clippy.toml` rejects `tock_registers::registers::{ReadWrite, ReadOnly,
    WriteOnly}` and raw `read_volatile`/`write_volatile`: they omit the
    device barriers.
  - For index/data register pairs (Super I/O, CMOS, SMN, IOAPIC), wrap the pair
    in `fstart_core::indexed::Indexed<I, D, R>`.
  - Format `register_bitfields!` blocks with `cargo ffmt --fix`.
- **Binary wire formats**: use `zerocopy` (`FromBytes`, `IntoBytes`,
  `Unaligned`, big/little-endian `U16`/`U32`/`U64`) over slices (`&[u8]` /
  `&mut [u8]`) rather than raw pointer arithmetic (see `crates/fdt`).
- **PCI resources**: describe every root bridge once via `fstart_pci::PciRole`
  (`Window`, `Forwarded`, `Kept`, `Own`) so the allocator, ACPI `_CRS`, and
  `PNP0C02` motherboard resources cannot drift.
- **ACPI AML**: author static and fixed-width-fixup AML with
  `fstart_acpi_macros::acpi_dsl!`.
- **Policy guards**: `clippy.toml` (barrier-aware MMIO) and
  `cargo fbuild checks` (`driver-amd` feature hygiene).

## Known Legacy Areas (Do Not Copy)

When adding new code, do not model it on these older areas—they are tracked for
cleanup:

- `crates/driver-intel`: `ich7/mod.rs`, `ich8/mod.rs`, `ich9/mod.rs` (partially
  extracted into `southbridge/`) and the untyped `MmioBar` raw-offset trait.
- `crates/driver-sunxi` & `crates/platform-sunxi`: per-SoC duplicated drivers
  and legacy SoC Cargo features.
