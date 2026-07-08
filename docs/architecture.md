# fstart Architecture: Config as Data, Fixed Family Flows, Few Crates

<!-- markdownlint-disable MD013 -->

This is the plan of record. It supersedes:

- [`rust-board-builder-stage-flow-plan.md`](rust-board-builder-stage-flow-plan.md)
- [`board-support-package-platform-recipe-plan.md`](board-support-package-platform-recipe-plan.md)
- `fstart-new/docs/reboot-architecture.md` (consolidated and revised here)

Still-valid pieces of those documents are folded in below. Everything else in
them is dropped, not deferred-by-silence; see
[Deliberately dropped](#deliberately-dropped).

## Goals

- Multiple boards per SoC, multiple SoCs, multiple ISAs, maximum code reuse.
- Board description is pure Rust. No devicetree, RON, YAML, or other
  intermediate format.
- Firmware without heap where possible; heap is acceptable once DRAM is up.
- coreboot-style flows (init hardware, build tables, jump to payload) and
  U-Boot-style flows (richer linked main environment, CrabEFI) from the same
  framework.
- Payload choice is not board identity. `fbuild` selects boot mode and payload
  files.
- Static typed firmware first. Dynamic board blobs are deferred, but the core
  decision below keeps that door open for free.

## Core decision: configuration is data

The single load-bearing decision. Everything else follows from it.

**Board and platform configuration is plain old data: `const`-evaluable,
`no_std`, serializable structs.** The fluent builder is syntax over that data,
nothing more. Stage code constructs live drivers *from* config at the right
point in a fixed flow. Config never contains driver objects, closures,
trait objects, or runtime state.

```rust
// Platform config: POD, a closed set of fields the chipset flow consumes.
// Builder methods are dumb const-fn data writes; build() checks invariants.
static PLATFORM: Gm965Ich8Config = Gm965Ich8Config::new()
    .sata(SataMode::Ahci, SataPorts::P0)
    .lpc(LpcDecode::new().com1().superio(io16(0x2e)))
    .pcie(Ich8RootPort::Port2, RootPortPolicy::enabled().wake_gpe(Gpe(13)))
    .build();
```

Because the whole chain is `const fn`, board config lives in ROM as a
`static`, costs no stack or RAM, and invariant violations in `build()` become
compile-time errors.

Rules for builders:

- A builder method writes data. If it does anything else, it is smuggling a
  device graph back in.
- **Litmus test: if the builder's output could not be postcard-serialized, it
  does not belong in the builder.** This is also what keeps dynamic board
  blobs cheap later: `derive(Serialize)` on config structs instead of a
  registry design now.
- `build()` validates invariants (exactly one console, port/decode
  consistency, address windows). Central validation code outside the owning
  platform crate is not allowed.

### Two config layers: closed platform data, open board code

The open set of board-attached devices (Super I/O, NICs, EC quirks, clock
generators) never enters platform data structures. That is the move that
avoids heap, type-list gymnastics, and a generic graph.

- **What the chipset flow must program is platform config**: closed, typed,
  POD. LPC decode windows, SATA mode, root port enables and policy, SMBus
  decode. The platform crate defines the struct; every field is something the
  platform's own flow reads.
- **What the board must do is hooks code**: open, board-owned. The board
  constructs its own devices from its own consts at the right hook point.

```rust
// The SuperIO *device* is board code, not platform data.
impl Gm965Ich8Hooks for X61Board {
    fn before_console(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<()> {
        Pc87392::new(io16(0x2e)).enable_com1(ctx)
    }
}
```

Where the platform needs to know *something* about an attached device (the
decode window for that Super I/O, the enable for that root port), that
something is a closed platform-typed config field. The device itself stays in
board code.

This is the same boundary coreboot found (chipset devicetree registers vs
mainboard code), and both existing repos already validated it: old fstart's
X61 `platform_config()` and fstart-new's hooks traits are each half of this
shape.

**Config lives in `.rodata`, never on the early-stage stack.** Early stages
run on tiny CAR/SRAM stacks. Board config is a `static`; the flow trait
exposes it as `const CONFIG: &'static Config`, derived per-driver configs are
const-evaluated associated consts (`const NB_CONFIG: &'static _ =
&Self::CONFIG.northbridge_config()`), and drivers hold `&'static Config` —
not by-value copies. No config transform functions may run at runtime in the
pre-DRAM path; if `objdump` shows a `*_config` function in the bootblock
`.text`, that is a regression.

## No generic device graph

`fstart-core` does not own a universal hardware graph, stringly properties,
`BusKind` taxonomies, or structural placeholder nodes. Board code fills
platform-typed config; platform flows consume it.

When mainstage needs an inventory (table generation, resource allocation), it
is generated from typed platform config plus hook contributions. A generic
property list is never the source of truth.

## Stage model

### Early stages: fixed per-family flows

This is the architectural improvement proven in the fstart-new prototype and
adopted here. There is **no** generic early-stage device lifecycle: no
16-method `HardwareInit` trait, no semantic flow entry table, no flow feature
families, no ordering DSL. Early stages are small, family-specific, and the
flow is handwritten once per platform family.

- Intel CAR has a fixed Intel flow.
- Sunxi SRAM has a fixed Sunxi flow.
- QEMU/simple boards have a fixed direct flow.

The family flow is generic over a board contract and a hooks trait with
default no-op methods:

```rust
pub trait IntelEarlyPlatform: IntelPlatform {
    type State: Default;

    fn run_early<B>(hooks: &mut B::Hooks) -> Result<EarlyHandoff>
    where
        B: IntelEarlyBoard<Platform = Self>;
}

pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform> {
    fn before_console(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<()> { Ok(()) }
    fn before_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<()> { Ok(()) }
    fn after_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<()> { Ok(()) }
    fn before_handoff(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<()> { Ok(()) }
}
```

Concrete chipset contracts live with the concrete chipset code and consume
static POD config:

```rust
pub trait Gm965Ich8Board: IntelEarlyBoard<Platform = Gm965Ich8> {
    fn config() -> &'static Gm965Ich8Config;
}
```

Everything is statically dispatched: no `dyn`, no type erasure, no codegen in
the pre-DRAM path. Default no-op hooks compile away.

Ordering is the handwritten family flow itself. If a board needs a different
order, that is a hook, or evidence the family flow is wrong and should be
fixed for everyone.

### Mainstage: phase trait, heap allowed

Once DRAM is up, a richer architecture-neutral model is fine. Heap and
`dyn MainstageDevice` are allowed here because the work is not a tight
bootblock path.

```text
bind fixed platform devices    # from typed config
pre_bus_scan                   # enable bridges/decode before enumeration
bus_scan                       # PCI/USB/enumerable buses
attach_dynamic                 # plug-in devices not in board config
assign_resources               # BARs, windows, IRQ routing
init_devices                   # what the selected boot mode needs
emit_tables                    # ACPI/FDT/SMBIOS, mostly from drivers
finalize                       # lock/quiesce
boot                           # calls the build-selected payload launcher
```

The trait is small and phase-oriented (`bind`, `pre_bus_scan`, `init`,
`emit_tables`, `finalize`). `MainstageCtx` owns real shared state (memory map,
resource allocator, PCI state, firmware volume, table builders), not an event
log and not a service locator. Fixed platform devices are driven by typed
config: the ICH SATA function uses the ICH SATA driver because the platform
config says so; PCI scan confirms presence and fills in BARs. Dynamic driver
matching is reserved for plug-in devices.

Payload launch is a separate abstraction selected by the build, not by the
platform recipe. The same mainstage flow can end in CrabEFI, FIT/Linux, direct
ELF, or a halt/test launcher; payload code consumes a small context exported by
mainstage instead of being `cfg(feature = "crabefi")` around mainstage itself.

Table generation lives close to driver code: a driver emits the standard
ACPI/FDT fragments for its own hardware; board-specific fragments stay in
board code and use typed references, not string paths.

### Stage granularity

Per family, decided by the family, stated explicitly:

- Intel: split (CAR bootblock/early stage, then DRAM-backed mainstage).
- Sunxi: SRAM early stage, DRAM mainstage.
- QEMU virt: monolithic is fine.

No generic multi-stage framework beyond what `fstart-stage` needs to enter a
stage and hand off.

## Boards

A board crate is the single owner of every board-specific fact: GPIO tables,
Super I/O config, HDA verbs, VBT selection, clock-gen programming, EC/dock
quirks, board ACPI fragments, SMM handlers, SMBIOS strings, flash layout
defaults. Per-board crates under `crates/` (the old
`fstart-mainboard-lenovo-x61` pattern) must not exist.

```text
boards/lenovo-x61/
  Cargo.toml            # [package.metadata.fstart] board/platform discovery keys + [[bin]] stanza
  src/lib.rs            # BoardSpec impl + hooks impls
  src/main.rs           # 3 fixed lines: fstart_stage::stage_bin!(...) — see below
  src/hw.rs             # static POD config
  src/acpi.rs           # board ACPI fragments (typed references)
  src/quirks.rs         # board hooks bodies
  src/host.rs           # cfg(feature = "host"): image template, blob defaults
  data/                 # VBT, board data
```

- Boards are real Cargo crates (rust-analyzer works) but excluded from the
  root workspace. `fbuild` discovers them via `boards/**/Cargo.toml` metadata
  and creates a temporary selected-board workspace under `target/`.
- Runtime modules contain no host paths, signing keys, or output names. Host
  concerns live behind `feature = "host"`.
- Payload files and boot mode are `fbuild` inputs, never board identity.
- Boards write no stage crate, no `StaticBoard` adapter, no registry entry,
  and no lifecycle forwarding impls. Board porting = POD config + hook impls
  plus the fixed stage entry declaration below.

### Stage entry binary

Cargo needs a `[[bin]]` target to produce the firmware executable, and since
Rust 2018 an `--extern` crate is only linked if referenced — so a shared
"empty bin" crate cannot pull in the selected board without either naming it
(a registry) or having its manifest generated (banned). The resolution: the
**board package owns the bin target**, and the entry code lives in one
`macro_rules!` macro in `fstart-stage`.

Per board, two fully declarative artifacts that never change after creation:

```rust
// boards/lenovo-x61/src/main.rs — exactly this, forever
#![no_std]
#![no_main]
fstart_stage::stage_bin!(fstart_board_lenovo_x61::Board);
```

```toml
# boards/lenovo-x61/Cargo.toml
[[bin]]
name = "fstart-stage"
path = "src/main.rs"
required-features = ["stage"]
```

`stage_bin!` expands to the `#[no_mangle] fstart_main` that dispatches into
the board's platform recipe, plus the `.fstart.keep` static that survives
`--gc-sections`. SMM uses the same pattern (`smm_bin!`, or the SMM entry
emitted from `stage_bin!` under `#[cfg(feature = "smm")]`) — no generated
SMM wrapper crate.

This is not the banned per-board boilerplate: the ban is on per-board stage
*logic* and generated wrappers. Three declarative lines naming the board type
once are config-as-data in Rust form — the only static, greppable,
rust-analyzer-visible edge from bin to board that needs no registry and no
generator. `fbuild` just runs
`cargo build -p fstart-board-lenovo-x61 --bin fstart-stage --features stage,...`.

Rejected alternatives, so they do not creep back:

- **Shared bin crate with per-board optional deps** — a central registry.
- **Generated wrapper crate (manifest-only or otherwise)** — generation; a
  phantom package rust-analyzer cannot see; already reintroduced once by
  accident, which is evidence the design invites relapse.
- **Platform-family stage crate** — the dependency points the wrong way;
  platform → board edges force a registry.
- **Building the board lib as `staticlib` + external link step** — `fbuild`
  becomes a linker driver outside cargo; loses rustflags/LTO/incremental,
  invisible to rust-analyzer.
- **`[[bin]] path` into a shared file outside the package, or a uniform
  `[lib] name = "board"` rename, or a proc macro reading a board-crate env
  var** — each fails on linkage, name collisions at 1000 boards, or
  rust-analyzer-hostile env magic.

### Configuration ownership

If a board author would copy the same value from a datasheet or reference
design into every port, it is a platform/chipset constant and lives in the
platform crate (RCBA base, MCHBAR, fixed ROM/SRAM windows, fixed IP block
bases). If changing the value is a meaningful board choice, it is board config
(console UART choice, DRAM population, GPIO routing, decode windows for board
parts, payload policy). Platform defaults are normal Rust constructors a board
starts from and overrides, never hidden global state.

Microcode is the canonical example: which microcode updates a chipset family
needs follows from the CPUs that family can carry, so the file list is a
platform fact — every Pineview board wants the same `06-1c-*` updates.
Boards do not restate it per port. Because file paths are host data, the
platform's microcode list is host-side platform code (behind
`feature = "host"` or consumed only by `fbuild`), never compiled into
runtime stage modules and never copied into board `host.rs`. A board
overrides the platform list only when its CPU population genuinely differs.

### Typed resources

Config fields use typed newtypes so misuse fails at compile time:

```rust
pub struct MmioAddr<T>(u64, PhantomData<T>);
pub struct IoAddr<T>(u16, PhantomData<T>);
pub struct Irq(pub u8);
pub struct Bdf { bus: u8, device: u8, function: u8 }
pub struct Gpe(pub u8);
```

`smbus_base(mmio32(...))` must not compile when SMBus base is an I/O port.

## Validation

- **Compile time**: typed newtypes, closed platform config fields, typed port
  enums (`Ich8RootPort::Port2`), hook trait signatures. Plus `build()` panics
  in `const` context = compile errors for invariant violations in `static`
  config.
- **`build()` time**: invariants the type system cannot express (one console,
  decode overlap, address windows), owned by the platform crate.
- **`fbuild` time**: target triple vs platform, missing payload/blob inputs,
  image layout fit.

No central validation crate. No runtime validation layer (that returns only
if dynamic blobs return).

## Crate organization

The old repo's disease is crate-per-chip: 60+ crates, seven for Sunxi alone
(`-sunxi-a20-dramc`, `-sunxi-ccu`, `-sunxi-d1-ccu`, `-sunxi-h3-ccu`,
`-sunxi-h3-dramc`, `-sunxi-mmc`, `-sunxi-spi`). Cargo crates are a unit of
compilation and reuse boundary, not a filing system. Modules are the filing
system.

**Rule: a new crate requires one of the following, otherwise it is a module
in an existing crate:**

1. A different `no_std`/host boundary (e.g. image reader vs image builder).
2. Genuinely independent reuse across platform families (e.g. NS16550).
3. Build-graph necessity (proc-macro, target-specific dependency isolation).

Target layout (~16 crates, from the fstart-new consolidation):

```text
Cargo.toml                    # workspace: crates/* and tools only; boards excluded
crates/
  fstart-core/                # BoardSpec/PlatformSpec, typed resources, errors (no_std)
  fstart-arch/                # per-ISA entry/asm/paging as modules: x86, arm, riscv
  fstart-stage/               # stage entry/runtime helpers, console install
  fstart-pci/                 # ECAM/CF8 access, BDF, scan, resource allocation (no_std)
  fstart-image/               # no_std FFS reader/hash verification
  fstart-image-build/         # host image assembly/signing (std)
  fstart-boot/                # payload launch: direct, FIT/Linux, CrabEFI
  fstart-acpi/
  fstart-fdt/
  fstart-platform-intel/      # Intel traits, config builders, early-flow entry
  fstart-driver-intel/        # gm965.rs ich8.rs pineview.rs ich7.rs q35.rs ...
  fstart-platform-sunxi/
  fstart-driver-sunxi/        # a20.rs h3.rs d1.rs ccu.rs dramc.rs mmc.rs spi.rs
  fstart-platform-qemu-virt/  # includes fw_cfg
  fstart-driver-uart/         # ns16550.rs pl011.rs sifive.rs
  fstart-driver-superio/      # pc87392.rs pc87382.rs ite8721f.rs
tools/
  fbuild/
boards/
  lenovo-x61/  foxconn-d41s/  qemu-riscv64/  ...
```

Vendor drivers group by vendor (`fstart-driver-intel`), small generic drivers
group by class (`fstart-driver-uart`, `fstart-driver-superio`). Chipset
knowledge splits as: generic family traits and entry points in
`fstart-platform-*`, concrete chipset sequences in `fstart-driver-*` —
matching the coreboot split where the common CAR wrapper calls into
northbridge/southbridge code.

`fstart-core` is `no_std` from day one. Host-only code lives in host crates
(`fstart-image-build`, `fbuild`) or behind `feature = "host"`, never mixed
into runtime modules.

## Build tool boundary

`fbuild` owns, exhaustively:

1. Board discovery from `boards/**/Cargo.toml` metadata.
2. Temporary selected-board workspace under `target/` — a workspace manifest
   listing the board package as a member, nothing more. No generated package
   manifests, no generated Rust.
3. Linker script emission and cargo invocation per stage (building the
   board-owned `fstart-stage` bin, see "Stage entry binary").
4. Flat-binary extraction and SoC image patching (eGON and friends).
5. Invoking `fstart-image-build` for FFS assembly, blobs, signing.
6. Boot mode / payload input selection (CLI overrides board host defaults).

Nothing else, ever. Image logic lives in `fstart-image-build` as a library;
`fbuild` stays a thin CLI. `fbuild` never generates Rust stage code and never
grows a `match board_name` registry. The old repo's 6k-line xtask is the
cautionary tale.

## Deliberately dropped

Dropped, with reasons, so they do not creep back:

- **Generic device graph / `DeviceEdge` / `BusKind` lowering** — the old
  repo's original sin; structural placeholder nodes and stringly typing with
  central validation. Replaced by closed platform config + open board hooks.
- **`HardwareInit` mega-trait, semantic flow entry table, flow feature
  families** — genericized early flow that no board needed. Replaced by fixed
  per-family flows.
- **Ordering DSL (`Order::before/after`), order hints** — ordering is the
  handwritten family flow.
- **Dynamic board-blob mode** — deferred. POD serializable config keeps it a
  `derive(Serialize)` away; no registry enums, derive macros, or runtime
  validation layers are designed until a deployment needs them.
- **Per-board stage crates, `StaticBoard` adapters, `fstart-mainboard-*`
  crates** — boards own facts and hooks only.
- **Live driver objects in builders** — the fstart-new prototype's builders
  discarded their arguments because there was nowhere for a
  `Pc87392::new(0x2e)` object to go without heap or a graph. That was the
  design rejecting live objects, not an unfinished stub.
- **`BuildInfo` mega-builder** — host build metadata is board `host.rs`
  defaults plus `fbuild` CLI, not a parallel metadata object model.

## Execution rule

**No parallel models, ever.** When a design changes, the old one is deleted in
the same change. The old repo died of tolerated transitional states (two stage
selection models, legacy xtask branches alongside generic ones), not of bad
design. Any transitional compatibility code must carry an explicit follow-up
plan or not be merged.

## Path forward

**Decided: the existing fstart repo is home**, adapted in place, because it
holds the hardware-init capital. The fstart-new prototype is retired; it
contributed the fixed per-family early-flow shape and the crate consolidation
target, and its code (hollow builders, std simulations) is not imported.

The cutover works on **one board: lenovo-x61**, whose stage closure is 41 of
the repo's 77 crates. Everything else leaves the workspace first.

1. **Shrink the world** (one mechanical commit). Workspace = X61 + its
   closure + xtask. Move out-of-closure hardware capital to `attic/`
   (browsable, not workspace members): all other boards, Sunxi (proven A20
   DRAM/MMC/eGON), Pineview/ICH7 (returns as board #2, foxconn-d41s), SiFive,
   and stray drivers/platforms. Delete xtask's dead wrapper branches for
   atticked boards.
2. **Rewrite the GM965/ICH8 path to this design.** Make `Gm965Ich8Config` a
   const-buildable POD `static`; replace the recipe/`StageFlow`/`StaticBoard`/
   capability-event machinery with the fixed Intel early flow + hooks trait
   and mainstage phases. Old model deleted in the same changes — no parallel
   models. This proves config-as-data on real chipset code. Payload-flavored
   recipe names (`Gm965Ich8UefiRecipe`, `stage-recipe = "gm965-ich8-uefi"`)
   die here too: the family flow is payload-agnostic; boot mode and payload
   inputs are CLI selections, and mainstage calls a common payload launcher
   trait rather than being gated by CrabEFI. Also migrate SMM to the
   board-owned entry pattern (see "Stage entry binary"): today
   `fstart-smm-image` builds `fstart-smm-stage` with a
   `FSTART_SMM_PLATFORM` env var that build.rs turns into board-name cfgs — a
   central board registry in cfg form — and binds `NoBoardSmmHandler`, leaving
   the X61 dock SMM handler dead. The board's own entry must bind its handler
   and the env/cfg selection must go.
3. **Host metadata cut.** Delete the `BoardInfo`/`BuildInfo` object model and
   the `fstart-codegen` board loader; xtask slims to the fbuild boundary
   (Cargo.toml metadata, selected-board workspace manifest, linker emission,
  image assembly).
4. **Mechanical crate consolidation** to the target layout, last, as safe
   churn: `fstart-driver-intel` ← gm965+ich8+gpio-ich+pmio-ich+smbus-intel+
   microcode+ck505; `fstart-driver-superio` ← superio+pc87382+pc87392;
   `fstart-pci` ← pci+ecam+driver-pci-ecam; `fstart-arch` ← arch+arch-x86+
   lapic+mp+cpu-intel; `fstart-core` ← types+mmio+pio+services remnants.
5. **Boards return one at a time** from `attic/`, ported to the new shape:
   foxconn-d41s (Intel early flow on second chipset, EM100 workflow), then a
   QEMU board for CI speed, then Sunxi. A board returns only by implementing
   the new contracts; no attic code is re-added unported.

Known gap, accepted: GM965 cold-boot DDR2 training is scaffolding. The
architecture cutover does not depend on it; do not measure the cutover by
X61 cold boot until raminit is finished.
