# fstart Architecture: Config as Data, Fixed Family Flows, Few Crates

<!-- markdownlint-disable MD013 -->

This is the target architecture, revised from the
[architecture review](https://uwkm2gle6nsv.postplan.dev). It supersedes the earlier
board-builder/stage-flow, BSP/platform-recipe and fstart-new plans. Target
interfaces below are not claims that the cutover is implemented. See
[Implementation and acceptance](#implementation-and-acceptance) for the gates.

## Goals

- Scale to hundreds of platforms and thousands of boards through change locality.
- New boards on supported hardware change their own directory and, when needed,
  the generated inventory/committed build lock. No central registry.
- Rust owns hardware policy; Cargo metadata owns build geometry and host inputs.
  No devicetree, RON, hardware graph or execution-order DSL.
- Fixed handwritten family flows construct live drivers from static config.
- Shared IP-block fixes reach all supported variants through one implementation.
- Unrelated SoCs do not enter an existing board's compilation closure.
- Release builds are the reference configuration. Build-only, emulator-booted
  and hardware-booted are separate support statuses.

## Configuration is data

Board/platform hardware configuration is const-evaluable `no_std` data. Builders
write fields and validate invariants; they do not create drivers, closures,
trait objects or runtime state. Closed platform config describes the hardware
its flow programs; open board hooks handle board-attached devices and quirks.

```rust
static PLATFORM: Gm965Ich8Config = Gm965Ich8Config::new()
    .sata(SataMode::Ahci, SataPorts::P0)
    .lpc(LpcDecode::new().com1().superio(io16(0x2e)))
    .build();
```

Config lives in `.rodata`, not on the early CAR/SRAM stack. Board contracts expose
`const CONFIG: &'static Config`; driver configs are const-derived and borrowed,
not reconstructed or copied at runtime before DRAM. `build()` enforces hardware
invariants in the owning platform, not in a central validator. Dynamic board
blobs remain deferred; serialization is not a reason to invent a registry.

Typed addresses (`IoAddr<T>`, `MmioAddr<T>`, `Irq`, `Bdf`, `Gpe`) are config data,
not live backends. Construct register accessors at runtime and retain
`tock-registers` register/bitfield definitions. Peripheral bases and decode choices
remain Rust even though executable placement belongs to metadata.

## Ownership and layering

| Fact | Authoritative source | Consumers |
| --- | --- | --- |
| DRAM policy, GPIO, pinmux, device config, SMBIOS strings | Board Rust and typed platform defaults | Static config and runtime table builders |
| Board/variant identity and platform reference | Board Cargo metadata | Discovery, selection, build report |
| Target, entry mode, stage structure, fixed ROM/SRAM windows and default budgets | Platform build-profile metadata | Resolved build and its projections |
| Flash geometry and partition map | Board metadata, optionally a platform image profile | Assembler and runtime layout view; no duplicate board `FLASH` |
| Stage load ranges, stacks, heaps and reservations | Platform profile plus permitted overrides | Linker symbols and runtime descriptor |
| Detected DRAM and usable memory | Memory-init results | Existing runtime map/handoff, intersected with reservations |
| Microcode/blob defaults and payload files | Platform/board metadata plus explicit CLI inputs | Host assembler only |
| Final offsets, lengths and compression results | Image assembly | Existing image directory/descriptor mechanisms |
| Hardware initialization order | Platform Rust | Handwritten calls, never a metadata step list |

```text
boards/<vendor>/<board>        hardware policy, wiring, hooks, board tables
    ↓                         metadata: identity, layout overrides, blobs
platform-<family>              flows, contracts, pairing, vendor entry
    ↓                         metadata: shared layouts and build selections
 driver-<vendor|class>         IP-block mechanisms and real hardware variants
    ↓
core / arch / pci / acpi / fdt / image reader / boot / smm

fbuild + image-build (host)    discover, resolve, link, assemble, explain
```

`fstart-stage` owns entry/runtime glue and stage-local layout access. Payload
launch belongs in `fstart-boot`; image loading stays in existing image facilities.
The layout wire definition belongs in a small `fstart-core::layout` module, not a
new crate. Host metadata types belong in fbuild/image-build, not core.

Crates are compilation/reuse boundaries, not a count target. A new crate needs a
real host/runtime boundary, independent reuse or target/dependency isolation.
Gate actual modules and dependencies, not just feature declarations. Optional
`std` support for real tests/tools is fine; firmware closures remain `no_std`.

## Build/runtime boundary

### Discovery and resolution

Scan `boards/*/*/Cargo.toml`, including packages excluded from the root workspace.
Parse TOML into typed, versioned host metadata; reject unknown fstart keys and
ambiguous identities. Never compile boards to discover them. Use Cargo metadata
to resolve the selected package's actual dependency graph and platform package.
Cache discovery by manifest contents when needed, not by compiling all boards.

Metadata composition is limited to one family layout referenced by a concrete
platform profile. Resolve family → platform → permitted board overrides →
permitted variant overrides → explicit CLI inputs. No recursive inheritance,
arbitrary stage-list override or inferred feature bag. Preserve field origins for
`fbuild explain`. Validate supported target/entry/payload combinations, direct
feature references, paths, overflow, overlaps and capacity before compilation.

IFD images import partition geometry through the existing image-format parser
from a descriptor input, or declare an explicit map from which image-build makes
the descriptor. Never keep both maps as independent authorities. Non-IFD images
use explicit geometry. Check supplied blobs against resolved regions.

### One immutable resolved build, three projections

```text
manifest discovery + platform profiles + inputs + CLI
                         ↓
                 ResolvedBuild (host)
                   /     |      \
             linker   runtime   image-build
             script    bytes     inputs
```

`ResolvedBuild` is not a board-authored builder or an execution model. It records
explicit compiler selections and geometry. `resolved-build.json` is a diagnostic
projection, never another editable configuration source.

1. Allocate fixed capacities/reservations before compiling; serialize each stage's
   runtime subset. Capacity covers image/BSS, stack and heap subranges.
2. Emit the linker script from the same value, including entry symbols, layout
   bytes and `ASSERT` checks for section/reservation budgets.
3. Compile the stage and validate actual ELF load/section ranges against the plan.
4. Assemble with the same plan and check compressed/uncompressed and partition
   limits. Oversized stages fail; they are not silently relocated.

There is no build-size cycle: reserved ranges are known before linking, while
actual file offsets are finalized by the existing assembler. No geometry inferred
from a first-stage address, duplicated Rust constants or growing env protocol.

### Linker-embedded descriptor

Use explicit-width little-endian bytes with magic, version, encoded length,
bounded counts, stage identity and relevant physical ranges/reservations. Host
encoding lives in image-build; core provides an allocation-free borrowed decoder.
Share wire constants and test agreement, malformed lengths/counts and overflow.
Decode byte fields, never cast to a native Rust struct.

The linker emits a retained, allocated, loaded `.fstart.layout` section with
`BYTE(...)` directives and `_fstart_layout_start`/`_fstart_layout_end` symbols.
Place it in initialized read-only storage, not BSS/debug space. Early assembly
uses absolute symbols such as `_fstart_stack_top`; Rust uses one stage-local
accessor that encapsulates the unsafe linker boundary and validates the bytes.
Prove retention in ELF and flat binaries and relocation on position-independent
stages before adopting the mechanism there.

The descriptor is **not** an inter-stage handoff, image directory or authentication
authority. [Authenticated boot](authenticated-boot.md) remains authoritative;
do not redesign loading or SMM packaging as part of this migration.

Detected RAM must contain planned RAM reservations. Exclude reservations and
platform MMIO apertures from the usable map at runtime. This validates actual
hardware, rather than inventing a second host hardware model.

### Selection and artifacts

Cargo features provide additive hardware/backend availability. Selection uses
three explicit target cfgs, supplied consistently to relevant target crates:

| cfg | Meaning |
| --- | --- |
| `fstart_stage_env` | One environment, including the existing SMM stage mode |
| `fstart_entry` | One architecture/vendor entry mode from the profile |
| `fstart_payload` | One terminal launcher, independently of enabled backends |

Register allowed cfg names/values; also reject missing/multiple selections and
selected backends without their feature. `--check-cfg` alone cannot enforce this.
Early stages need not compile payload backends. Do not mirror cfgs into env vars.

Feature selections refer to the board's actual direct dependency keys and
existing features. Shared propagation stays inside platform crates; boards only
own variant features, not forwarding features for every shared capability.
Multiple payload backends may coexist with one selection. Do not require global
`--all-features` across incompatible architectures or board variants.

Key output directories by board/variant, stage, target, profile, selections and
resolved-layout digest. A digest-specific `-T` path forces layout-only changes to
relink. Find outputs through Cargo JSON artifact messages, not a guessed shared
binary path. Concurrent boards must not overwrite artifacts.

## Board entry and flows

Boards remain real Cargo packages with checked-in sources and board-owned bins:

```text
boards/<vendor>/<board>/
  Cargo.toml       identity, platform, geometry overrides, blob inputs, bin
  src/lib.rs       family board contract and hooks
  src/main.rs      shared entry macro with platform-owned adapter
  src/hw.rs        static hardware data
  src/quirks.rs    board-specific orchestration
  src/acpi.rs      board-specific table contributions when needed
  data/           board blobs
```

The target has no board host executable for layout discovery. The current
`host`/`BoardConfig` path is removed in each migrated scope, not duplicated.

Proposed entry contract (to be proved by the first vertical slice):

```rust
// fstart-stage
pub trait StageProgram {
    fn run_stage(handoff: usize) -> !;
}

// platform-intel owns the adapter, so this satisfies Rust's orphan rules.
pub struct IntelProgram<B>(core::marker::PhantomData<B>);
impl<B: IntelBoard> fstart_stage::StageProgram for IntelProgram<B> {
    fn run_stage(handoff: usize) -> ! {
        crate::run_stage::<B>(handoff)
    }
}

// board bin: no forwarding impl or platform → board dependency
fstart_stage::stage_bin!(
    fstart_platform_intel::IntelProgram<fstart_board_lenovo_x61::Board>
);
```

Do not implement a foreign stage trait for an uncovered generic board type in
a platform crate. SMM adapters follow the same ownership rule while preserving
its existing entry contract.

Family flows remain handwritten: Intel CAR/postcar/ramstage, Sunxi SRAM/DRAM,
and simple QEMU direct flows. The current Intel flow can cover GM965, i945 and
Pineview; split it only for demonstrated sequencing differences, not hypothetical
future generations. No generic early-device lifecycle or ordering DSL.

Bootblock and ramstage hooks are distinct traits; Sunxi SRAM/DRAM hooks likewise.
Document prerequisites, available memory/console/buses and cold/warm/resume
execution points. Never reuse an early hook accidentally to reconstruct mainstage
state. Mainstage owns the real memory map, PCI/resource and table state. Heap
and dynamic plug-in drivers are acceptable after DRAM, not prerequisites for
fixed hardware. ACPI/FDT contributions live near drivers; board fragments and
SMBIOS policy stay in board Rust.

Variants share one package only when hardware structure and flow are shared.
Use explicit exactly-one variant selection with `--no-default-features`; reject
zero/multiple variants where a board requires one. Defaults are editor convenience,
not firmware selection. Divergent flows indicate separate boards.

## Reuse proofs

- **Intel:** `I945Ich7` and `PineviewIch7` compose different northbridges with the
  **same Ich7 driver implementation**. Pairing modules own closed config, defaults
  and type associations, not copies of ICH7 initialization. GM965/ICH8 is the
  adjacent-generation comparison. Share verified ICH7/ICH8 mechanisms, not
  sequences assumed equivalent from similar APIs. The current flow may use
  I801/ICH-specific services without pretending to cover all future Intel.
- **Sunxi MMC:** inventory A20/H3/D1 FIFO, timing, calibration, clock/reset and DMA
  differences before sharing IP algorithms. Use typed variant enums/validated
  constants or narrowly named variant operations, not a boolean matrix allowing
  impossible combinations. Similar DRAMC files do not prove reusable algorithms.
- **Board devices:** datasheet register sequences for Super I/O, EC, clock/dock
  devices belong in drivers. Board hooks own wiring, policy and actual quirks.
- **SiFive:** reusable FU740 hardware code moves out of the board into the
  appropriate platform/driver owner.

Drivers may compose through public device/bus services, including PCI and SMBus;
they do not discover boards or select/load stages, payloads or firmware images.
For host tests, start with pure calculations and existing mockable buses. Add
only the smallest access boundary a meaningful sequence test needs, retaining
register definitions and modeling relevant side effects/ordering. A write trace
does not validate electrical behavior or raminit.

## Cargo lock and editor contract

Keep boards excluded from the root development workspace. The target is one
**discovered resolution/build workspace** containing all board/runtime packages,
with one committed `build-support/boards.lock`. Release invocation selects one
package, not `--workspace`. Generate only workspace/editor config and necessary
source links, never package manifests or Rust wrappers.

`fbuild lock` intentionally refreshes the committed build lock. Normal builds
seed the generated workspace lock from it and use `--locked`. The root workspace
may retain its own lock for its different inventory. Pin toolchain and record
external blob digests and compiler settings as well; a Cargo lock is insufficient
for reproducibility.

The all-board workspace is **not** automatically the editor project. Both
`fbuild ide <board> --stage <stage> --payload <payload>` and `fbuild check` consume
the same resolved selection as build. Generate a bounded active-board view with
real sources, target, features, exact cfgs and proc-macro/build-script settings.
Analysis and check-on-save must agree; a check command alone is insufficient.
Preserve unrelated user editor settings and document reload when switching.

Before workspace cutover, prove:

- Real-source navigation through entry macro, adapter, platform and driver;
  completion, macro expansion and matching editor/check type errors.
- Switching stage/variant/payload and architecture clears stale cfgs and identities.
- Edits reach original sources; symlink canonicalization and workspace membership
  do not duplicate or misidentify packages.
- The active view does not analyze unrelated boards; measure indexing as inventory
  grows. `cargo check -p` alone does not bound rust-analyzer analysis.
- Any disposable development lock preserves selected versions, sources and
  checksums from the canonical lock. A copied full lock is not assumed to work.
- Regeneration from clean checkouts preserves package identity and lock stability.

One active selection per editor session is sufficient. If a selected-source
workspace cannot meet these constraints, derive an alternative view from Cargo
metadata while retaining proc-macro/build-script support. Do not delete working
editor paths or board locks until these gates pass.

## Implementation and acceptance

This revision starts from source revision `38586a3e`; the review's original
`8009aceb` audit counts are historical. Initial source budgets and fresh command
results belong in [the migration baseline](architecture-baseline.md).

Current progress: discovery and the QEMU RISC-V vertical slice are integrated,
including halt/Linux/CrabEFI release boots, metadata-only geometry/relink proof,
the platform entry adapter, additive-backend checks and selection-aware `check`.
The bounded editor view and canonical lock prototype remain unproven; workspace
ownership has not cut over. Other boards retain their previous build path.

1. **Boundary and discovery:** record ownership, selection, descriptor, adapter and
   lock decisions. Replace textual manifest parsing with typed/versioned TOML and
   globally unique board/variant identities. This does not migrate build geometry.
2. **QEMU vertical slice:** metadata → ResolvedBuild → linker bytes/symbols → runtime
   view → assembler. Prove geometry-only changes reach all consumers, no host board
   executable, descriptor retention and a release boot.
3. **Cargo/editor proof in that slice:** implement the adapter, direct dependency
   selections, additive payload checks, isolated artifacts, `ide`/`check` and common
   lock prototype. Workspace cutover remains blocked by the gates above.
4. **Intel build boundary:** prove multistage reservations; remove the superseded
   host layout path, board `FLASH` and inferred features in the migrated scope.
   Do not rewrite authenticated loading or hardware register sequences.
5. **Pairing proof:** i945/ICH7 and Pineview/ICH7 share southbridge operations;
   release-build both and preserve available boot evidence.
6. **Flow/port cleanup:** stage-specific hooks, adapters, no shared-feature
   forwarding, shared Sunxi hook vocabulary and reusable FU740 ownership.
7. **Sunxi MMC:** consolidate verified mechanisms with focused variant checks and
   available boots; consider CCU next, not automatic DRAMC consolidation.
8. **Validation/cleanup:** full discovered release matrix and representative boots;
   remove superseded models, SMBIOS duplication and locks only after replacement.
   Preserve unported attic hardware knowledge rather than mechanically deleting it.

Every migrated merged scope has one authoritative path. Intermediate development
may be staged, but must name its next cutover and must not claim a prototype is
proven. No backward-compatibility framework or permanent dual model.

Retain existing QEMU coverage, especially q35 → CrabEFI → GRUB → Linux, and
[authenticated-boot validation](authenticated-boot-validation.md). X61 cold-boot
DDR2 training exists but the previous architecture record still awaited observed
cold boot on hardware; this revision does not upgrade that status. Do not equate
successful linking or emulator boots with hardware validation.

Record clean/incremental release time, actual compilation closure, image sizes and
stack-budget/high-water evidence on representative boards. Source stack budgets
are not measurements of stack usage. Run affected/representative boards for each
change and the complete declared release matrix periodically and before releases.
