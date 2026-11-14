# AMD silicon: IP blocks, SoCs, boards

fstart's AMD code is split along three axes. Each has one home.

| Axis | Owns | Lives in |
|---|---|---|
| IP block and its versions | registers, sequences, generated vendor tables | `crates/driver-amd` |
| SoC | which version of each block, in which order, with which constants; its place in the boot flow | `crates/platform-amd` |
| Board | policy, lanes, pads, devices, the stage `main` | `boards/*` |

## driver-amd: IP blocks

One module per IP block. Common code sits at the top of the block; a
submodule per IP version holds what the versions do differently. Versions
are named by AMD's IP number where it is public and by the die's code name
otherwise; product names (Naples, Rome, ...) do not appear.

| Block | Common | Versions |
|---|---|---|
| `df` | registers, `Df` (one node), `Fabric` (every node), `topology`, `plan`, `program`, `map`, `dram` | `v2`/`v3` layouts; `rv`, `zp`, `ssp`, `gn` init |
| `nbio` | `iohc`, `ioapic`, `iommu`, `nbif`, `pcie`, `fuse`, `table`, `Nbio` (a root bridge's NBIO) | `rv`, `zp`, `ssp` |
| `smu` | mailbox, thread release | `v9`, `v10`, `v11` |
| `dxio` | firmware client, `wire`, `lanes`, `encode`, `ports`, `hotplug` | `rv`, `zp` (`zp::sequence`), `ssp` |
| `ccx` | MSRs, microcode, MP, cache, power, the table engine (`table`) and the vendor sequence (`init`) | `rv`, `zp`; `tables::{zp, ssp, gn}` |
| `fch` | registers over a `Bus` (an SoC's FCH 0), over windows for any FCH (`hw`), the shared tasks (`hw::tasks`) | `sandstone`, `taishan`, `huashan` |
| `psp`, `apob` | mailbox and commands, the APOB | |

`cpu::Silicon` (`Zp`, `Ssp`, `Gn`) is what IP code tells apart when a
sequence depends on the die; everything a SoC decides is an argument or a
policy value. What a block needs to know of the chip it is part of is the
`soc::Soc` trait, which the platform's SoCs implement. `testing` holds the
host models of the dies.

driver-amd has no features for SoCs or IP versions: it builds every block,
and the linker keeps what a board's SoCs reach (release builds use LTO,
executables link with `--gc-sections`). Its features are for optional
dependencies only (`acpi`, `smbios`, `smm`, `spi-flash`, `testing`).

## platform-amd: SoCs

One module per SoC or SoC family: `picasso`, `zeppelin`, `sp3` (Naples,
Rome, Milan). A SoC module says which IP versions it runs, in which order
(`soc::Init`), with which constants (ECAM, layout, APOB, packages), and
what the flow needs of it beyond the silicon (`flow::Soc`). `flow` is the
boot flow every SoC shares; `noncar` the services the SoCs made of dies
share.

platform-amd has no SoC features either. A board's stage calls the flow of
the SoCs it carries, so only those are linked; an SP3 board matches
`sp3::start()` and halts on the families it does not list.

## Adding a SoC

1. In driver-amd, add the IP versions that are new (a `df::v4`, an
   `nbio::rs`, ...), next to the existing ones. Existing versions do not
   change.
2. In platform-amd, add the SoC's module: its `Soc` and `Init`, its layout
   and its `flow::Soc`.
3. In the board, call the SoC's flow from the stage `main`.

## Rules

- driver-amd never names a SoC: no product names in modules, types or
  matches, no SoC features.
- A difference between versions is data if it is a number, address or
  table, and a function in the version's module if it is a sequence.
- Common code is the default; a version module exists only where two
  versions differ.
- Every register has one typed definition (`register_bitfields!`, see
  `AGENTS.md`).
- Generated vendor tables are checked in as Rust with their provenance;
  their generators are not part of the tree.
