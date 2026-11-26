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
| `df` | registers, `Df` (one node), `Fabric` (every node), `topology`, `plan`, `program`, `map`, `dram` | `v2`/`v3` layouts; `rv`, `zp`, `ssp`, `gn` init; `v4` (RS: its own `Df`, IOS, bus maps, rules); `wh` (DFv4.5: a node per IOD, 8-bit fabric IDs, 16/32/16 pairs, its device tables) |
| `nbio` | `iohc`, `ioapic`, `iommu`, `nbif`, `pcie`, `fuse`, `table`, `Nbio` (a root bridge's NBIO) | `rv`, `zp`, `ssp`, `rs` (`Ios`: an IOS's NBIO), `wh` |
| `smu` | mailbox, thread release | `v9`, `v10`, `v11`, `v13`, `v14` (WH: its messages, CPPC, USB, brand string) |
| `dxio` | firmware client, `wire`, `lanes`, `encode`, `ports`, `hotplug` | `rv`, `zp` (`zp::sequence`), `ssp`; `rs` (MPIO: `ask`, `mailbox`, `session`, `ports`); `wh` (MPIO v2: lanes and layout, port descriptors, global config, session per IOD, straps, hotplug, the `PCIE_PROXY` 0x26 `Proxy`, the ABL-trained `early` link) |
| `ccx` | MSRs, microcode, MP, cache, power, the table engine (`table`) and the vendor sequence (`init`), MCA setup and the banks' errors at boot (`mca`), MCA work in SMM: thresholds, their polling in a storm, PFEH (`ras`) | `rv`, `zp`, `rs`, `wh`; `tables::{zp, ssp, gn, rs, wh}` |
| `fch` | one handle for every FCH (`FchHw`, FCH 0 as `Native`), one register set per block, the SMI dispatcher and its periodic SMI (`smi`) and sleep path (`sx`), the SP3 vendor tasks (`tasks`), the SPI flash (`spi`, its SMN reader) and ROM3 (`rom3`) | `sandstone`, `taishan`, `huashan`, `songshan`, `qilian` (takeover, HW-ACPI, eSPI windows, SPI TPM, secondary FCHs, USB, S5) |
| `psp`, `apob` | C2P mailbox and commands, the P2C mailbox an SMM handler answers (`p2c`), the APOB (with the ABL's event log and the errors in it, `apob::event`) | `psp::Variant::Wh` (the WH mailbox), `psp::ras` (Rome's RAS-EINJ commands), `psp::einj` (the RS and WH PSPs' own EINJ service, WH's `WhBuffer`), `apob::rs` (the RS records that differ from Milan's), `apob::v2` (WH's APOB V2), `apob::cxl` (its CXL address map) |
| `cxl` | the host's CXL steps every SoC with CXL takes (`host`: windows, HDM, CEDT inputs), the MPIO CXL requests (`mpio`), the error namespace | `wh` (WH's ports, RCRB, RCEC, QoS) |

`cpu::Silicon` (`Zp`, `Ssp`, `Gn`) is what IP code tells apart when a
sequence depends on the die; on WH, `cpu::wh::Part` is the one CPUID
decode every block's choice (PCIe layout, DF device table, NBIO gates)
derives from. Everything a SoC decides is an argument or a policy value. What a block needs to know of the chip it is part of is the
`soc::Soc` trait, which the platform's SoCs implement. `testing` holds the
host models of the dies.

driver-amd has no features for SoCs or IP versions: it builds every block,
and the linker keeps what a board's SoCs reach (release builds use LTO,
executables link with `--gc-sections`). Its features are for optional
dependencies only (`acpi`, `smbios`, `smm`, `spi-flash`, `testing`).
`fbuild checks` enforces this feature list, including features Cargo creates
implicitly for optional dependencies. Use `dep:` to keep an optional
dependency behind an approved capability rather than exposing it as a
separate feature.

## platform-amd: SoCs

One module per SoC: `picasso`, `zeppelin`, `naples`, `rome`, `milan`,
`genoa`, `siena`, `venice` (via shared `sp7`).
A SoC module says which IP versions it runs, in which order
(`soc::Init`), with which constants (ECAM, layout, APOB, packages), and
what the flow needs of it beyond the silicon (`flow::Soc`). SoCs that share
their steps share a module: `iod` holds the steps of Rome and Milan, which
take their die as a type (`iod::Variant`), and `rs` those of Genoa and
Siena, the SoCs of the RS I/O die (`rs::Variant`: the IOS a part has, its
DRAM layout, its SMU and PM base); `noncar` what the SoCs made of dies
share; `sp3` what the SoCs of the SP3 socket share (its flash halves, its
memory reservations, the flow's services for the socket); `sp7` what the
SoCs of the SP7 socket share (`sp7::Variant`; WH dual IOD, 1P and 2P).
What SoCs of several families share has its own module: the board's
soldered devices (`links`, for the SoCs with MPIO) and a root bridge's
non-PCI MMIO allocator (`nonpci`).

The flow (`flow`) is the same for every SoC, and so are its services, each
of which takes the SoC's data: the reset layout (`layout`), the memory map
(`memory`), the boot media (`media`), MP (`mp` with the driver's
`ccx::mp::Fam17hCpu`), the fabric (`soc::Fabric`: the PCI segment and the
DF plan's root bridges, the PCI roots), legacy VGA (one display, chosen
once: the one behind the port the SoC names, Picasso its GPU's bridge,
or a board's lanes attach its onboard display to, else the first
VGA-compatible one; only the bridges to it forward the ranges, the SoC
routes the DF's VGA window to its root bridge, `Init::route_legacy_vga`,
and a video ROM is posted on it), SMM installation (`tseg`) and its
handler (`smm`: one SMRAM configuration and installer, the handler on the
driver's SMI dispatcher, which a SoC with SMM work of its own extends, as
`sp3::smm` and `rs::smm` do, what the PSP needs of SMM for the SoCs
that register with it, `smm::psp`, and the machine-check handler of the
SoCs that run one, `smm::ras`), options (`options`), measured boot
(`tpm`: the flow starts it right after the memory map for every SoC and
board with a discrete TPM, measuring the reset image the SoC finds in
flash, `flow::Soc::reset_image`), the ACPI tables (`acpi::install`, from what the SoC gathers; the
HEST and the BERT of the last boot's machine checks and of the ABL's
errors, `acpi::apei`) and the SMBIOS
builder, and the host build plan (`host`, with the image kinds
`host::single` and `host::dual`).

A board implements `board::Board`: its name, the SoCs it carries, its CPU
ceiling, its TPM and BMC hooks, its RAS policy (whether its BMC channel
is the OS's too) and the function that reports a RAS event to its BMC. Its stage `main` matches the SoC
`flow::start` returns and enters that SoC's flow with its configuration
of it; the SoCs it does not handle halt, and only the flows it calls are
linked. platform-amd has no SoC features either.

What a board tells the ABL (its APCB tokens and CXL regions) is image
build data in its `[package.metadata.fstart.amd]` table, written into the
APCBs it packs: see docs/amd-apcb.md.

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
- A SoC's code in platform-amd that is not generic over the SoC holds no
  string (no log, `expect` or panic message), or is `#[inline]`: the
  linker keeps a crate's merged strings with the ones another SoC uses, so
  such a string changes every other board's image. Driver code is not
  affected the same way, but keep new IP versions in their own module.
