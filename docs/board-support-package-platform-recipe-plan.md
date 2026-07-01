# Board Support Package and Platform Recipe Plan

<!-- markdownlint-disable MD013 -->

This document is a follow-on to
[`rust-board-builder-stage-flow-plan.md`](rust-board-builder-stage-flow-plan.md).
That plan establishes the direction of Rust board metadata and fixed handwritten
stage flow. This plan tightens the ownership and scaling model needed to support
hundreds of boards without board-local stage glue or artificial per-board
`mainboard` crates.

## Problem statement

The current Rust board direction is better than RON/generated-stage glue, but the
implementation can still split one board's hardware truth across multiple crates
and force each board to hand-write stage adapters.

The Lenovo X61 is the clearest example:

- `boards/lenovo-x61/src/lib.rs` owns platform metadata, flash layout, payload
  policy, SMBIOS metadata, and parts of the GM965/ICH8 hardware configuration.
- `crates/fstart-mainboard-lenovo-x61/src/lib.rs` owns other X61 hardware facts:
  IGD config, GPIO tables, HDA config, CK505 config, SuperIO config, dock/DLPC
  init, ACPI, SMBIOS runtime descriptors, and SMM.
- `boards/lenovo-x61/stage/src/*.rs` owns stage adapter glue that constructs
  devices and wires fixed-flow hooks.

That split does not scale. A board port should not need to decide whether a GPIO
table belongs in a `boards/` crate or a board-specific crate under `crates/`.
There should be one owner for board-specific hardware facts.

The Sunxi boards showed the same pattern in another form: multiple board stage
crates repeated nearly identical bootblock/main-stage code because the reusable
family recipe was missing.

## Goals

- Make each board crate the single source of truth for board-specific hardware
  facts, quirks, and metadata.
- Delete artificial per-board `mainboard` crates unless the code is genuinely
  shared across multiple boards.
- Move common stage sequencing into reusable platform recipe crates.
- Remove committed per-board stage adapters where a recipe can generate or select
  the same entrypoint shape.
- Keep stage flow handwritten and readable, but place it at the recipe/platform
  level rather than copying it into every board.
- Preserve static typed mode as the primary path.
- Keep dynamic board-blob mode possible, but do not let it complicate static BSP
  ergonomics.
- Keep board-specific quirks out of generic framework code. Generic framework code
  should not learn concepts like `southbridge`, `dock`, or `mainboard`.

## Non-goals

- Do not introduce a macro-heavy board DSL.
- Do not return to generated per-board stage control flow.
- Do not introduce a central runtime `match board_name` registry in the firmware
  stage.
- Do not force every board to have a `mainboard` abstraction.
- Do not keep a separate crate just because a board has a lot of board-specific
  code.
- Do not optimize for preserving current crate boundaries.

## Design principles

### Board-specific facts live in the board crate

A board crate is a board support package, or BSP. It owns all facts that are only
true for that board.

Examples:

- GPIO tables and pin routing.
- HDA verbs and codec routing.
- IGD/VBT selection.
- SuperIO configuration.
- CK505/clock-generator programming.
- EC, dock, mux, and board strap quirks.
- Board-specific ACPI fragments.
- Board-specific SMM handlers.
- SMBIOS strings and tables.
- Flash layout and image packaging policy.
- Payload default policy.

Reusable device drivers and reusable chipset/platform defaults stay outside the
board crate.

### Platform recipes own reusable sequencing

A platform recipe is a reusable stage-flow implementation for a family of boards.
It is parameterized by a board trait. It owns the common bootblock, ramstage,
payload, handoff, and platform sequencing for that board family.

Examples:

- `Gm965Ich8UefiRecipe<B>` for GM965/ICH8 UEFI-style boards.
- `PineviewIch7UefiRecipe<B>` for Pineview/ICH7 UEFI-style boards.
- `SunxiMmcLinuxRecipe<B>` for Allwinner boards that boot from MMC to Linux.
- `QemuVirtLinuxRecipe<B>` for QEMU virt LinuxBoot boards.
- `Q35UefiRecipe<B>` for Q35/UEFI boards.

The recipe is handwritten Rust. It is not generated stage code. The point is to
write the flow once per platform family instead of once per board.

### Board crates implement recipe traits

A board should mainly implement the trait required by its selected recipe. For an
X61-like board, that trait supplies board facts and board-specific hooks to the
GM965/ICH8 recipe.

A board port should look like:

```rust
pub struct Board;

impl FirmwareBoard for Board {
    type Recipe = Gm965Ich8UefiRecipe<Self>;

    const NAME: &'static str = "lenovo-x61";
    const PLATFORM: Platform = Platform::X86_64;

    fn build_info() -> BuildInfo {
        config::build_info()
    }
}

impl Gm965Ich8UefiBoard for Board {
    type Mainboard = X61Mainboard;

    fn platform_config() -> Gm965Ich8Config {
        config::gm965_ich8_config()
    }

    fn mainboard() -> Result<Self::Mainboard, ServiceError> {
        X61Mainboard::new(devices::x61_mainboard_config())
    }

    fn console_config() -> Ns16550Config {
        devices::uart0_config()
    }

    fn smbios() -> &'static SmbiosDesc<'static> {
        &config::X61_SMBIOS_DESC
    }
}
```

No per-board `impl StaticBoard` should be needed when a recipe exists.

### The selected-board stage wrapper is build glue, not generated flow

Per-board stage crates are still too much boilerplate at 100s of boards.
Instead, `xtask` should create a temporary selected-board wrapper package under
`target/fstart-build/`.

The wrapper aliases the selected board package to a stable crate name:

```toml
[dependencies]
fstart-board-selected = { package = "fstart-board-lenovo-x61", path = "../../../boards/lenovo-x61" }
fstart-stage-template = { path = "../../../crates/fstart-stage-template" }
```

The stage template has a generic entrypoint:

```rust
use fstart_board_selected::Board;

#[no_mangle]
pub extern "Rust" fn fstart_main(handoff: usize) -> ! {
    fstart_stage_template::run::<Board>(handoff)
}
```

This is acceptable because it does not generate board-specific control flow. It
only selects a type. The flow remains handwritten in the selected board's recipe.

## Target crate layout

### Top-level layout

```text
boards/
  lenovo-x61/
  foxconn-d41s/
  bananapi-m1/

crates/
  fstart-driver-*/
  fstart-platform-*/
  fstart-stage-template/
  fstart-stage-runtime/
  fstart-services/
```

### Lenovo X61 BSP layout

```text
boards/lenovo-x61/
  Cargo.toml
  src/
    lib.rs          # exports Board and board public API
    config.rs       # board_info, build_info, flash layout, payload policy, SMBIOS metadata
    devices.rs      # IGD, GPIO, HDA, CK505, SuperIO, UART config
    mainboard.rs    # X61Mainboard, dock, DLPC, i8042, X61 ACPI
    smm.rs          # X61 SMM handler
```

The following crate should be deleted:

```text
crates/fstart-mainboard-lenovo-x61/
```

### Reusable GM965/ICH8 platform layout

```text
crates/fstart-platform-intel-gm965-ich8/
  src/
    lib.rs
    config.rs
    recipe.rs
```

`recipe.rs` owns the reusable bootblock and ramstage recipe for static typed
GM965/ICH8 boards.

## Trait shape

### Firmware board trait

```rust
pub trait FirmwareBoard: Sized + 'static {
    type Recipe: StageRecipe<Self>;

    const NAME: &'static str;
    const PLATFORM: Platform;

    fn board_info() -> BoardInfo;
    fn build_info() -> BuildInfo;
}
```

This trait is generic. It should not mention x86, Sunxi, PCI, ACPI, FDT, or
payload details.

### Stage recipe trait

```rust
pub trait StageRecipe<B: FirmwareBoard> {
    fn run(stage: StageKind, handoff: usize) -> !;
}
```

`StageKind` is selected by build metadata and the temporary selected-board
wrapper. A multi-stage board can dispatch to bootblock or ramstage. A monolithic
board can ignore unsupported stages or fail at build validation.

### GM965/ICH8 UEFI board trait

```rust
pub trait Gm965Ich8UefiBoard: FirmwareBoard {
    type Mainboard: Gm965Ich8Mainboard;

    fn platform_config() -> Gm965Ich8Config;
    fn mainboard() -> Result<Self::Mainboard, ServiceError>;
    fn console_config() -> Ns16550Config;
    fn smbios() -> &'static SmbiosDesc<'static>;
}
```

### GM965/ICH8 board hook trait

```rust
pub trait Gm965Ich8Mainboard {
    fn pre_console(&mut self, ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        let _ = ich8;
        Ok(())
    }

    fn post_dram(&mut self, ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        let _ = ich8;
        Ok(())
    }

    fn finalize(&mut self, ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        let _ = ich8;
        Ok(())
    }

    #[cfg(feature = "acpi")]
    fn dsdt_aml(&self, ctx: &AcpiContext) -> Vec<u8>;
}
```

This trait is deliberately platform-specific. It lives in the GM965/ICH8 platform
crate, not in `fstart-services`. The common framework does not gain a generic
`with_southbridge()` concept.

## Static typed flow with recipes

The GM965/ICH8 recipe can own concrete stage devices:

```rust
pub struct Gm965Ich8RamstageDevices<B: Gm965Ich8UefiBoard> {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: B::Mainboard,
    console: StaticConsole<Ns16550>,
    memory: X86MemoryState,
    acpi_rsdp: Option<u64>,
}
```

The recipe owns generic platform sequencing:

```rust
impl<B> HardwareInit for Gm965Ich8RamstageDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console(ctx)?;
        self.southbridge.pre_console(ctx)?;
        self.mainboard.pre_console(&mut self.southbridge)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.memory.detect(&self.northbridge)
    }

    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.init_bus()?;
        self.southbridge.ramstage_init()?;
        self.mainboard.post_dram(&mut self.southbridge)?;
        self.init_mp()
    }

    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.prepare_acpi();
        self.prepare_smbios();
        Ok(())
    }
}
```

The board supplies facts. The platform recipe supplies ordering. The driver
supplies hardware operations.

## ACPI and topology paths

ACPI should not hard-code absolute paths forever. A recipe can provide an ACPI
context derived from the platform topology:

```rust
pub struct AcpiContext<'a> {
    topology: &'a DeviceTopology,
    names: &'a AcpiNameMap,
}

impl AcpiContext<'_> {
    pub fn path(&self, device: DeviceName) -> AcpiPath;
    pub fn scope_for(&self, device: DeviceName) -> AcpiScope;
}
```

For the first migration, X61 ACPI can move as-is into `boards/lenovo-x61`, but the
long-term target is that board ACPI emits fragments using recipe/topology-provided
paths rather than string literals like `"\\_SB_.PCI0.LPCB"`.

## SMM ownership

If an SMM handler is specific to one board, it belongs in that board crate.

For X61:

```text
boards/lenovo-x61/src/smm.rs
```

`crates/fstart-smm-stage` currently depends on X61-specific code. That is a sign
that SMM stage ownership is also wrong. There are two migration choices:

1. Short-term: keep `crates/fstart-smm-stage`, but make it depend on
   `fstart-board-lenovo-x61` instead of `fstart-mainboard-lenovo-x61`.
2. Long-term: move board-specific SMM stages to selected-board recipe/wrapper
   build flow, just like bootblock and ramstage.

The short-term option is acceptable during the BSP merge. The long-term option is
consistent with the selected-board stage template model.

## Cargo feature model

### Board crate features

The board crate should own features for board-specific optional code:

```toml
[features]
default = []
runtime = [
  "dep:fstart-pio",
  "dep:fstart-arch-x86",
  "dep:fstart-driver-nsc-pc87382",
  "dep:fstart-driver-nsc-pc87392",
  "dep:fstart-driver-i2c-ck505",
  "dep:fstart-gpio-ich",
  "dep:fstart-hda",
  "dep:fstart-smbios",
]
acpi = ["runtime", "dep:fstart-acpi", "dep:fstart-acpi-macros"]
smm = ["runtime", "dep:fstart-smm-runtime"]
```

### Stage features

Stage features should select platform recipe/backend capabilities, not
board-specific crates:

```toml
[features]
gm965-ich8-uefi = ["dep:fstart-platform-intel-gm965-ich8"]
flow-profile-multistage = [...]
acpi = [...]
smbios = [...]
```

The selected board crate is a normal dependency of the selected-board wrapper.
The generic `fstart-stage` crate should not have one optional dependency per
board.

## Build flow

The build flow for a static typed board should be:

1. `xtask build --board lenovo-x61` scans `boards/*/Cargo.toml`.
2. It builds or runs board metadata code to obtain `BuildInfo`.
3. It selects the board package and stage recipe features.
4. It creates a temporary selected-board stage package under `target/fstart-build`.
5. The temporary package depends on the board crate as `fstart-board-selected`.
6. The generic stage template calls `Board::Recipe::run(stage, handoff)`.
7. The recipe runs handwritten platform flow.

This gives static dispatch without committed per-board stage glue.

## Dynamic board-blob mode compatibility

Dynamic board-blob mode remains possible, but it is not the primary design driver.

Static BSP mode:

- Board is a Rust type.
- Recipe is a Rust type.
- Device config is strongly typed.
- Stage flow is monomorphized and optimized.

Dynamic mode:

- Board crate can emit a serialized topology/config blob from the same builders.
- The stage uses a compiled-in driver registry.
- Runtime validation checks topology, feature availability, and config ABI.

The important rule is that dynamic mode should reuse `BoardInfo`/topology data,
not force static BSPs to look like dynamic registries.

## Migration plan

### Phase 1: X61 BSP ownership cleanup

- Move `crates/fstart-mainboard-lenovo-x61/src/lib.rs` into:
  - `boards/lenovo-x61/src/devices.rs`
  - `boards/lenovo-x61/src/mainboard.rs`
  - `boards/lenovo-x61/src/smm.rs`
- Move X61-specific runtime SMBIOS descriptor into `boards/lenovo-x61/src/config.rs`
  or `mainboard.rs`.
- Update X61 board and stage imports to use `fstart_board_lenovo_x61` only.
- Change `fstart-smm-stage` to import X61 SMM code from the board crate.
- Delete `crates/fstart-mainboard-lenovo-x61` from the workspace.
- Remove all workspace dependencies and feature forwarding for the deleted crate.

Acceptance criteria:

- There is no `fstart-mainboard-lenovo-x61` crate.
- All X61-specific config lives under `boards/lenovo-x61`.
- `boards/lenovo-x61/src/lib.rs` no longer imports X61 facts from another X61 crate.
- `cargo run -p xtask -- build --board lenovo-x61` passes.

### Phase 2: GM965/ICH8 recipe

- Add `Gm965Ich8UefiBoard` and `Gm965Ich8Mainboard` traits to
  `fstart-platform-intel-gm965-ich8`.
- Move common X61 bootblock/ramstage sequencing into
  `Gm965Ich8UefiRecipe<B>`.
- Make `boards/lenovo-x61::Board` implement the recipe trait.
- Reduce X61 stage files to temporary type aliases or delete them once the selected
  wrapper exists.

Acceptance criteria:

- X61 does not hand-write `impl StaticBoard` for bootblock or ramstage.
- X61 does not hand-write generic GM965/ICH8 init ordering.
- X61 board code only supplies X61 facts and hooks.

### Phase 3: Selected-board stage template

- Add `crates/fstart-stage-template`.
- Teach `xtask` to create a selected-board temporary wrapper package.
- Build X61 through the wrapper.
- Delete `boards/lenovo-x61/stage` once equivalent artifacts are produced.

Acceptance criteria:

- Adding a board does not require committing a `stage/` crate.
- The selected wrapper contains only board selection, not stage flow.
- Stage flow remains handwritten in recipe crates.

### Phase 4: Generalize to existing families

- Convert Sunxi boards from board-local stage crates to `SunxiMmcLinuxRecipe<B>`.
- Convert QEMU virt boards to `QemuVirtLinuxRecipe<B>`.
- Convert Q35 to `Q35UefiRecipe<B>`.
- Convert Pineview/ICH7 boards to `PineviewIch7UefiRecipe<B>`.

Acceptance criteria:

- Board dirs contain BSP code and metadata, not repeated stage adapters.
- Platform recipes cover repeated sequencing.
- Board addition cost is dominated by board facts, not framework glue.

## Expected end state

For 100s of boards, adding a board should usually require:

```text
boards/new-board/Cargo.toml
boards/new-board/src/lib.rs
boards/new-board/src/config.rs
boards/new-board/src/devices.rs
boards/new-board/src/mainboard.rs   # only if board quirks exist
```

The board author should not need to write:

- A stage crate.
- A bootblock `StaticBoard` adapter.
- A ramstage `StaticBoard` adapter.
- A board-specific crate under `crates/`.
- A generic device lifecycle forwarding impl.
- A central registry entry.

The long-term architecture should be:

```text
Board crate:     owns board facts and quirks.
Platform recipe: owns family sequencing.
Drivers:         own reusable hardware operations.
Stage template:  owns generic entrypoint and selected-board dispatch.
xtask:           owns build/package selection.
```

That is the cleanest split for scaling to hundreds of boards while keeping Rust
board definitions explicit and reviewable.
