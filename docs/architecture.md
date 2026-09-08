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
- Rust owns hardware policy and typed board/image facts; platform Rust derives
  shared build geometry. Cargo metadata selects identity and the platform export.
  No devicetree, RON, Cargo hardware schema or execution-order DSL.
  QEMU RISC-V, ARMv7 and AArch64 select typed platform-owned machine presets
  and share the concrete platform-plan executor.
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
`tock-registers` register/bitfield definitions. Peripheral bases and decode choices remain Rust. Intel executable placement is
shared platform policy, not repeated board geometry.

## Ownership and layering

| Fact | Authoritative source | Consumers |
| --- | --- | --- |
| DRAM policy, GPIO, pinmux, device config, SMBIOS strings | Board Rust and typed platform defaults | Static config and runtime table builders |
| Board/variant identity and platform reference | Board Cargo metadata | Discovery, selection, build report |
| Target, entry mode, stage structure and default budgets | Platform Rust policy | Concrete compiler units and their projections |
| Flash chip capacity and partition map | Typed board facts where physical boards vary (Intel); platform invariants for supported QEMU virt presets | Host resolver, assembler and linked runtime descriptor |
| Stage load ranges, stacks, heaps and reservations | Shared platform Rust calculations and real chipset deltas | Linker symbols and runtime descriptor |
| Detected DRAM and usable memory | Memory-init results | Existing runtime map/handoff, intersected with reservations |
| Microcode/blob defaults and payload files | Platform Rust / board facts plus explicit CLI inputs | Host assembler only |
| Final offsets, lengths and compression results | Image assembly | Existing image directory/descriptor mechanisms |
| Hardware initialization order | Platform Rust | Handwritten calls, never a metadata step list |

```text
boards/<vendor>/<board>        hardware policy, wiring, hooks, board tables
    ↓                         metadata: identity and platform export
platform-<family>              flows, contracts, pairing, vendor entry
    ↓                         Rust: shared layouts, Cargo stage bundles
 driver-<vendor|class>         IP-block mechanisms and real hardware variants
    ↓
core / arch / pci / acpi / fdt / image reader / boot / smm

fbuild + image-build (host)    discover, resolve, link, assemble, explain
```

`fstart-stage` owns entry/runtime glue and stage-local layout access. Payload
launch belongs in `fstart-boot`; image loading stays in existing image facilities.
The layout wire definition belongs in a small `fstart-core::layout` module, not a
new crate. Host plan transport and reservation types belong in image-build, not core.
Platform-specific calculations stay in the platform, not in fbuild.

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

Intel/X61 uses `build-profile = { dependency = "fstart-platform-intel", name = "rust" }`.
The dependency is resolved through Cargo's actual graph (including renames), not
through a chipset registry in fbuild. The selected board exposes `Board` and
implements the platform's small host-clean facts trait. Its `const` IFD value
calls `IntelIfdFlashLayout::new`; the facts constructor checks the separately
declared physical chip capacity and CPU population. VBT and hardware config
remain typed board source, active in both firmware and editor graphs.

The platform's optional host module exports the conventional `Plan<B>::emit(selection_json)`.
The generated adapter passes an explicit serialized `BuildSelection` argument
to `platform::Plan::<board::Board>::emit`;
Cargo owns its dependency graph, build scripts, proc macros and linking. It emits
image-build's family-free `ResolvedPlan`/`BuildPlan` transport: named compiler
units with their own target, Cargo target kind, cfgs, features, flags, generated
linker text, ELF expectations and artifact bindings, plus concrete image inputs.
Defaults, supported payloads and terminal-stage assignment belong to platform
Rust; fbuild qualifies Cargo aliases, validates references and executes the same
bounded unit contract for build, check and IDE. This is not a generated authoring API,
board host feature/executable or firmware recipe. The Intel family calculates
common capacities once; the tested i945 comparison changes only the real CAR
window, without migrating that board's legacy build path.

QEMU virt boards select `VirtMachine::{Riscv64, Armv7, Aarch64}` in unconditional
Rust source. The platform owns invariant flash banks, fixed reservations and
compiler bundles, sharing one image projection while keeping explicit XIP,
direct ARM Linux and AArch64 relocation differences. AArch64 no longer repeats
an artificial fixed flash-capacity fact in board source. Other board-host builds
remain legacy; there is no claim that all fbuild paths are generic. Imported IFD/host transport and
selected reservations retain checked validation; const authoring is not a reason
to skip ELF and assembly validation. See the [common-plan boundary and acceptance](architecture-common-plan.md).

### One immutable resolved build, three projections

```text
manifest discovery + typed board facts + platform calculation + inputs + CLI
                         ↓
                 ResolvedBuild (host)
                   /     |      \
             linker   runtime   image-build
             script    bytes     inputs
```

The concrete resolved plan is not a board-authored builder or hardware execution
model. It records compiler units, fixed geometry and a bounded producer-artifact
dependency relation, not a lifecycle DSL. All units and bindings are validated
before execution; named selections build only their required producer closure.
Linker, ELF and format helpers are selected by platform Rust, not by family
matches in fbuild. `resolved-build.json` is a diagnostic projection, never another
editable configuration source.

1. Allocate fixed **runtime** capacities/reservations before compiling; serialize
   each stage's runtime subset. Capacity covers image/BSS, stack and heap
   subranges. Physical flash banks/BIOS partitions are storage policy, not
   preallocated per-file slots.
2. Emit the linker script from the same value, including entry symbols, layout
   bytes and `ASSERT` checks for section/reservation budgets.
3. Compile the stage and validate actual ELF load/section ranges against the plan.
4. Assemble with the same plan and check compressed/uncompressed and partition
   limits. Oversized stages fail; they are not silently relocated.

Intel XIP placement uses single-link section-size arithmetic at the BIOS top;
RAM-stage initialized data is compact, with BSS/heap/stack protected separately.
The assembler measures actual artifacts and packs files using compression and
alignment, rejecting partition overflow. No fixed bootblock budget is subtracted
from filesystem capacity. This corrects the earlier fixed-storage interpretation;
it does not change runtime protection or restore the removed metadata resolver.

Linking needs no measure/relink cycle. Constant verification policy is finalized
before compression; mutable locators occur only in uncompressed initial storage
and are transported separately to successors. Compressed locator inputs fail
packaging, rather than triggering layout/compression convergence. Sunxi's fixed
finalizer seals its checksum before the initial digest and final directory/root.
RISC-V virt packs into one physical 32-MiB bank; ARM's two 64-MiB banks remain.
See [phase2a contracts and evidence](architecture-common-plan.md#locatortrust-separation-phase2a).
This is not root-first slots, A/B recovery, hardware secure boot or rollback.

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
  Cargo.toml       identity, platform reference, real dependencies/variants, bin
  src/lib.rs       family board contract and hooks
  src/main.rs      shared entry macro with platform-owned adapter
  src/hw.rs        static hardware data
  src/quirks.rs    board-specific orchestration
  src/acpi.rs      board-specific table contributions when needed
  data/           board blobs
```

Intel/X61 has no board host feature or executable. Its generated host adapter
only calls the shared platform export; the legacy `host`/`BoardConfig` path
remains solely on unmigrated boards.

Current X61 entry uses a platform-owned adapter and hygienic macro:

```rust
fstart_platform_intel::stage_bin!(
    fstart_platform_intel::gm965::Program<fstart_board_lenovo_x61::Board>
);
```

`Program<B>` implements `fstart_stage::StageProgram` inside its owning platform.
The board no longer relays `stage`, `runtime`, `smm` or payload Cargo features.
Platform `bundle-bootblock`, `bundle-postcar`, `bundle-ramstage`, `bundle-smm`
features activate real shared dependencies. The ramstage bundle includes ACPI,
MP and SMBIOS; UEFI selects the direct platform dependency's `payload-uefi`.
Board-imported Lenovo/UART/ACPI-macro drivers remain ordinary direct dependencies.
SMM uses the platform's shared SMM export and `fstart_stage_env="smm"`; its
current-artifact-only producer still runs before ramstage and embeds exact bytes.

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

ACPI is the normal Intel ramstage policy, owned by the platform ramstage bundle rather
than a per-board opt-in. Boards supply their table fragments and hooks; any
Cargo dependency activation needed to compile them is not a board policy
selector. Hardware initialization (including Lenovo EC/PMH7 bring-up) is
independent of table emission. A future explicit bring-up profile may omit
ACPI without omitting that hardware initialization; do not add such a profile
until needed. The legacy board Cargo forwarding feature disappears with that
board's metadata cutover, not through a second interim selection mechanism.

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

### Current Rust-plan packaging (not a workspace ownership cutover)

The pre-existing selected-source preparation still copies the root lock and lets
Cargo metadata resolve it; this change does **not** establish end-to-end
zero-resolution builds. After that preparation, the host adapter adds exactly
one deterministic local runner package entry, preserving every prepared package
entry and pin verbatim. Adapter compilation uses `--locked` and fails closed;
there is no resolution fallback. No board locks are removed, no audit workspace
is promoted and no new committed lock authority is introduced. Native linking,
proc macros, toolchain/profile compatibility and dependency aliases stay Cargo's
responsibility, not a hand-built rustc driver.

Host and firmware use separate target directories and feature selections. Typed
board facts are unconditional original-source modules, not host-only cfgs. The
existing Cargo-derived editor/check views still select one real stage graph.
Generating a new view needs valid host facts; an already generated view can
report original-source errors without regenerating the plan. Host compilation
alone is not evidence of live editor behavior.

### Future workspace cutover (unchanged, separate work)

Keep boards excluded from the root development workspace. The target is one
**discovered resolution/build workspace** containing all board/runtime packages,
with one committed `build-support/boards.lock`. Release invocation selects one
package, not `--workspace`. Generate workspace/editor config and necessary source links, not board package
manifests or board-authoring interfaces. The thin shared host adapter described
above is an executable shim only.

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
The [Cargo-derived editor view](ide.md) now passes original-source navigation,
proc macros, compiler diagnostics and payload/profile switching. Its all-board
lock prototype agrees with the tested selected compiler graphs. ARMv7 now uses
that same resolved path, with ELF32 validation, halt/Linux release boots and live
RISC-V ↔ ARMv7 editor switches passing. AArch64 now has distinct flash-storage,
RAM-execution and writable reservations, validated relocation extents, and passing
halt/Linux/CrabEFI release boots from the earlier relocation milestone. X61's
typed board IFD/CPU facts and all three QEMU virt machine selections now feed the
**same concrete platform-plan executor** through Cargo host exports. Intel
multistage, RISC-V/ARMv7 XIP and AArch64 relocation require no family branches in
common build/check/IDE tools. The former virt Cargo geometry profiles and their
metadata resolver have been deleted; independent captured output fixtures retain
the behavior proof. The remaining ten boards still use BoardConfig host callbacks. Fresh halt/UEFI X61 assembly, exact descriptors/SMM/microcode,
AArch64 halt assembly/boot, and live car → postcar → ram → SMM → AArch64 editor
switching with original-source invalid-fact diagnostics pass. Hardware boot and
stack high-water measurements remain outstanding. Workspace ownership has not
cut over, and explicit legacy routes remain for other boards. See
[common-plan acceptance and scope](architecture-common-plan.md) and the earlier
[Intel typed-facts acceptance](architecture-intel-typed-facts.md).

1. **Boundary and discovery:** record ownership, selection, descriptor, adapter and
   lock decisions. Replace textual manifest parsing with typed/versioned TOML and
   globally unique board/variant identities. This does not migrate build geometry.
2. **QEMU vertical slice:** metadata → ResolvedBuild → linker bytes/symbols → runtime
   view → assembler. Prove geometry-only changes reach all consumers, no host board
   executable, descriptor retention and a release boot.
3. **Cargo/editor proof in that slice:** implement the adapter, direct dependency
   selections, additive payload checks, isolated artifacts, `ide`/`check` and common
   lock prototype. Workspace cutover remains blocked by the gates above.
4. **Intel build boundary:** prove multistage reservations; replace duplicated
   metadata geometry with const-validated board facts and shared platform policy.
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

### Intel IFD active-list correction and image safety

The immediate metadata baseline declared descriptor/GbE/ME/BIOS, but its
`ConstVec::new(first)` conversion failed to push the descriptor into the active
list. X61's typed constructor explicitly pushes all four regions. This is a
separate active-list validation bug fix, not byte-identical restoration of the
old serialized list. Descriptor overlap now participates in validation.

The existing assembler is unchanged: its generated 4-MiB firmware image has
erased (`0xff`) non-BIOS ranges. It does not contain a populated factory IFD,
GbE MAC/settings or ME firmware and is not a factory/full-chip backup. **Do not
blindly flash the entire generated image onto hardware.** This scope adds no
opaque blob import, manufacture or replacement. Runtime flash geometry still
comes from the linker-embedded descriptor, never by independently evaluating
board facts in firmware.
