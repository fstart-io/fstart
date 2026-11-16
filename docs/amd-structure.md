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
| `fch` | one handle for every FCH (`FchHw`, FCH 0 as `Native`), one register set per block, the SMI dispatcher (`smi`) and sleep path (`sx`), the SP3 vendor tasks (`tasks`) | `sandstone`, `taishan`, `huashan` |
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

One module per SoC: `picasso`, `zeppelin`, `naples`, `rome`, `milan`.
A SoC module says which IP versions it runs, in which order
(`soc::Init`), with which constants (ECAM, layout, APOB, packages), and
what the flow needs of it beyond the silicon (`flow::Soc`). SoCs that share
their steps share a module: `iod` holds the steps of Rome and Milan, which
take their die as a type (`iod::Variant`); `noncar` what the SoCs made of
dies share; `sp3` what the SoCs of the SP3 socket share (its flash halves,
its memory reservations, the flow's services for the socket).

The flow (`flow`) is the same for every SoC, and so are its services, each
of which takes the SoC's data: the reset layout (`layout`), the memory map
(`memory`), the boot media (`media`), MP (`mp` with the driver's
`ccx::mp::Fam17hCpu`), the fabric (`soc::Fabric`: the PCI segment and the
DF plan's root bridges, the PCI roots), SMM installation (`tseg`) and its
handler (`smm`: one SMRAM configuration and installer, the handler on the
driver's SMI dispatcher, which a SoC with SMM work of its own extends, as
`sp3::smm` does), options (`options`), measured boot (`tpm`), the ACPI
tables (`acpi::install`, from what the SoC gathers) and the SMBIOS
builder, and the host build plan (`host`, with the image kinds
`host::single` and `host::dual`).

A board implements `board::Board`: its name, the SoCs it carries, its CPU
ceiling, its TPM and BMC hooks. Its stage `main` matches the SoC
`flow::start` returns and enters that SoC's flow with its configuration
of it; the SoCs it does not handle halt, and only the flows it calls are
linked. platform-amd has no SoC features either.

## Adding a SoC

1. In driver-amd, add the IP versions that are new (a `df::v4`, an
   `nbio::rs`, ...), next to the existing ones. Existing versions do not
   change.
2. In platform-amd, add the SoC's module: its `Soc` and `Init`, its layout
   and its `flow::Soc`, and its `board::SocId`.
3. In the board, list the SoC in `Board::SOCS` and call its flow from the
   stage `main`.

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
