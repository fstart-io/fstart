# Topology-derived ACPI scope placement design

## Summary

The current worktree contains the wrong direction for ACPI scope placement: it adds per-board `acpi_parent` string references and then uses those strings to wrap driver AML. That recreates the pre-nested-RON problem that `ron_loader` was designed to remove, and it still leaves hardcoded `PCI0`/absolute-scope assumptions inside drivers.

Clean direction: make the nested board topology the only parent relation, give ACPI-relevant topology/structural nodes optional ACPI NameSegs, and make drivers emit AML fragments relative to a codegen-provided scope. Codegen should derive all scope paths from the flattened topology and the driver/structural-node ACPI names. No board should say `acpi_parent: "northbridge"`, and drivers should not emit `Scope("\\_SB_.PCI0")` to find their parent.

## Evidence from the current code

### Topology loading already has the right foundation

- `crates/fstart-codegen/src/ron_loader.rs:105-144` documents and parses nested `children`; `ron_loader` says the tree structure is the hierarchy and should not need parent string references.
- `crates/fstart-codegen/src/ron_loader.rs:257-340` flattens devices in pre-order DFS and records each parent as an index in `DeviceNode`.
- `crates/fstart-types/src/device.rs:43-56` defines `DeviceNode { parent, depth }` as an index-based flat device tree.
- `crates/fstart-types/src/device.rs:82-85` still carries a string `parent` only as derived metadata for compatibility, but the real relationship in codegen is `DeviceNode.parent`.

### Current in-progress ACPI parent approach is the part to reject

- The worktree adds `DeviceConfig.acpi_parent` at `crates/fstart-types/src/device.rs:90-96` and parses it from RON at `crates/fstart-codegen/src/ron_loader.rs:148-154` / `323-329`.
- `RuntimeDeviceTable::acpi_parent_index()` at `crates/fstart-codegen/src/stage_gen/board_gen/model.rs:275-284` resolves that string by scanning entries by name. This reintroduces ad hoc cross references.
- `boards/foxconn-d41s/src/lib.rs:75-96` and `boards/lenovo-x61/board.ron:53-80` now set `acpi_name: "PCI0"` on the northbridge and `acpi_parent: "northbridge"` on the southbridge. These are workaround annotations, not topology.
- `crates/fstart-codegen/src/stage_gen/board_gen/caps_tables.rs:135-143` wraps driver AML with `scoped_aml_with_root_fragments(parent_path, ...)`, where `parent_path` may be derived from `acpi_parent`.

### ACPI assembly currently assumes unscoped device AML goes under `\_SB_`

- `fstart-acpi::device::AcpiDevice` says `dsdt_aml()` returns serialized AML and “the caller places the returned bytes inside a `\_SB` scope” (`crates/acpi/src/device.rs:27-41`). This is too weak for nested bus scopes.
- `fstart-acpi::platform::build_dsdt()` always splits root fragments and wraps all normal AML in `\_SB_` (`crates/acpi/src/platform/mod.rs:393-428`).
- `RootScope` is already the right mechanism for true DSDT-root objects: it marks `Scope("\\")` content so the assembler can strip it and place it at DSDT root (`crates/acpi/src/lib.rs:65-95`, platform split at `crates/acpi/src/platform/mod.rs:436-464`).
- The new `scoped_aml_with_root_fragments()` helper (`crates/acpi/src/lib.rs:121-135`) can be kept as an implementation primitive, but the scope path must come from topology, not board string overrides.

### Driver AML is inconsistent today

- Pineview currently emits sibling `Device(MCHC)`, `Device(PDRC)`, and `Device("PCI0")` at one caller scope (`crates/fstart-driver-intel-pineview/src/lib.rs:1370-1508`). The board workaround sets topology `acpi_name: "PCI0"` while the Pineview config still uses `acpi_name: "MCHC"` (`boards/foxconn-d41s/src/lib.rs:75-91`). That is two conflicting meanings of “the device ACPI name”.
- GM965 is closer to the desired host-bridge shape: it emits one `Device(PCI0)` containing `MCHC`, `PDRC`, GFX, etc. (`crates/fstart-driver-intel-gm965/src/lib.rs:2112-2148`). But its root fragments `_PIC`/sleep states are appended as normal AML (`crates/fstart-driver-intel-gm965/src/lib.rs:2337-2350`), so if this driver ever has a parent wrapper, those root objects would be misplaced. They should use `RootScope`/`Scope("\\")`.
- ICH7 says the caller should embed its output inside the appropriate PCI0 scope (`crates/fstart-driver-intel-ich7/src/lib.rs:1972-1978`) and emits `Device(LPCB)` plus PCI function siblings as relative AML, but it also emits an absolute `Scope("\\_SB_.PCI0")` for RCRB (`crates/fstart-driver-intel-ich7/src/lib.rs:2054-2086`). That hardcoded root path must go.
- ICH8 currently emits PCI0-relative fragments as a local `pci0_aml` (`crates/fstart-driver-intel-ich8/src/lib.rs:2070-2223`), but its doc still says `\_SB.PCI0` (`2032`) and its device names (`LPCB`, `SATA`, `SBUS`, `RP01`...) are hardcoded.
- SuperIO ACPI says its nodes are nested by the assembler (`crates/fstart-superio/src/lib.rs:1030-1036`) and emits relative children like `COM1`, `KBC`, etc. This is the pattern to preserve.
- Lenovo X61 mainboard still hardcodes absolute `\_SB_.PCI0.LPCB...` paths and `Scope("\\_SB_.PCI0.LPCB")` (`fstart-mainboard-lenovo-x61/src/lib.rs:437-449`, `660-672`). It needs generated path/context APIs before it can be topology-derived.

## Proposed clean model

### 1. Schema/topology

Keep/add only this ACPI topology metadata:

- `DeviceConfig.acpi_name: Option<HString<8>>`: ACPI NameSeg for this topology node, used only when the topology node itself exists in the ACPI namespace.
- No `DeviceConfig.acpi_parent`. Delete/revert it from `fstart-types`, `ron_loader`, tests, and boards. Parentage is `DeviceNode.parent` only.
- Validate `acpi_name` as an ACPI NameSeg (1-4 chars, ACPI-safe characters). The current type allows 8 bytes, but ACPI NameSegs are 4 chars; either shrink to `HString<4>` or keep `HString<8>` temporarily and validate at load/codegen.

Add registry/codegen metadata, not board parent references:

```rust
pub enum AcpiContributionKind {
    None,
    /// Driver emits `Device(<this node's ACPI name>)` or equivalent relative to its parent scope.
    NamedNode,
    /// Driver is an aggregate/owner and emits child objects relative to its parent scope.
    ParentScopeFragment,
}
```

This is needed because several ACPI contributors do not have a meaningful self node:

- ICH7/ICH8 southbridge drivers own many PCI function namespace nodes (`LPCB`, `SBUS`, `RP01`, SATA, USB...) under the PCI root scope.
- SuperIO drivers emit logical-device children under LPCB, not a `SIO0` container in the current AML.
- Mainboard drivers may emit root objects, GPE methods, EC/dock fragments, etc.

Do not use fake `acpi_name` values just to make `acpi_runtime_devices()` include a driver. Inclusion should be based on driver metadata and board ACPI capability, not name-as-enable-flag.

### 2. Codegen context APIs

Build a dedicated `AcpiNamespaceModel` beside `RuntimeDeviceTable` rather than bolting path logic onto generic runtime devices.

Suggested APIs:

```rust
struct AcpiNamespaceModel<'a> { /* flattened devices + tree + instances */ }

impl<'a> AcpiNamespaceModel<'a> {
    fn node_name(&self, idx: usize) -> Option<&'a str>;        // topology acpi_name or driver primary name
    fn node_path(&self, idx: usize) -> Option<AcpiPath>;       // \_SB_.PCI0.LPCB, skips unnamed aggregate nodes
    fn parent_scope_path(&self, idx: usize) -> AcpiPath;       // nearest named ancestor, or \_SB_
    fn contribution_scope(&self, idx: usize) -> AcpiPath;      // normally parent_scope_path(idx)
    fn children(&self, idx: usize) -> impl Iterator<Item=AcpiNodeRef<'a>>;
    fn child_by_bus(&self, idx: usize, bus: BusAddress) -> Option<AcpiNodeRef<'a>>;
    fn child_by_service(&self, idx: usize, service: Service) -> Option<AcpiNodeRef<'a>>;
    fn path_by_device_name(&self, name: &str) -> Option<AcpiPath>; // for mainboard cross-references
}
```

Validation in this model should catch:

- duplicate `acpi_name` siblings under the same derived ACPI parent scope;
- an ACPI contributor whose `NamedNode` has no name;
- path construction through `acpi_parent` or any non-topology edge (should not exist);
- absolute scope AML in drivers except true root fragments (`RootScope` / `Scope("\\")`).

Generated runtime context for drivers:

```rust
pub struct AcpiDeviceContext<'a> {
    pub parent_scope: &'a str,
    pub node_name: Option<&'a str>,
    pub node_path: Option<&'a str>,
    pub children: &'a [AcpiChildContext<'a>],
}

pub struct AcpiChildContext<'a> {
    pub device_name: &'a str,
    pub acpi_name: Option<&'a str>,
    pub path: Option<&'a str>,
    pub bus: Option<BusAddress>,
    pub services: &'a [Service],
    pub enabled: bool,
}
```

Then evolve the trait to either:

```rust
fn dsdt_aml(&self, config: &Self::Config, ctx: &AcpiDeviceContext<'_>) -> Vec<u8>;
fn extra_tables(&self, config: &Self::Config, ctx: &AcpiDeviceContext<'_>) -> Vec<Vec<u8>>;
```

or add a new `AcpiDeviceV2` trait and adapt old drivers during migration. `extra_tables` also needs context: e.g. PL011 DBG2 currently hardcodes `\_SB.<acpi_name>` and should use `ctx.node_path`.

### 3. DSDT assembly flow

Move from “concatenate all AML and maybe wrap some manually” to “collect scoped fragments”:

```rust
struct ScopedAml<'a> {
    scope: AcpiScope<'a>, // Root or Path("\\_SB_.PCI0")
    bytes: Vec<u8>,
}
```

Implementation can still serialize through existing `RootScope` markers and `scoped_aml_with_root_fragments()`, but the higher-level API should be explicit:

1. For each ACPI contributor, derive `scope = namespace.contribution_scope(idx)`.
2. Call the driver with `AcpiDeviceContext`.
3. Split root fragments from relative fragments.
4. Append root fragments to DSDT root.
5. Group normal fragments by scope path and emit one `Scope(path)` per path (or append directly to the implicit `\_SB_` wrapper for `\_SB_`).

This avoids repeated wrappers and makes it testable that, for example, ICH7 contributes to `\_SB_.PCI0`, SuperIO contributes to `\_SB_.PCI0.LPCB`, and mainboard EC contributes to `\_SB_.PCI0.LPCB` without a board-level parent override.

### 4. Driver AML contract

Drivers should emit relative AML:

- A simple device emits `Device(<node_name>) { ... }` and never emits an absolute parent scope.
- A root bridge emits `Device(<node_name, e.g. PCI0>) { ... }` relative to `\_SB_`.
- A southbridge aggregate emits child objects relative to the PCI root scope: `Device(LPCB)`, `Device(SBUS)`, `Device(RP01)`, USB/SATA/HDA siblings, `Name(_PRT)`, etc. It should get `LPCB`/`SBUS`/`RPxx` names from the structural children in `AcpiDeviceContext`, with chipset defaults only as compatibility fallbacks during migration.
- Root-scope objects (`_PIC`, `_S0_`, `_S5_`, global operation regions that truly belong at root) must be emitted as `Scope("\\")` / `RootScope`, not as normal relative AML.
- No driver should emit `Scope("\\_SB_.PCI0")` or concatenate a hardcoded `PCI0` parent path.
- Mainboard drivers should use generated context paths for cross references. For Lenovo X61, replace `\_SB_.PCI0.LPCB.EC__...` string literals with `ctx.path_by_device_name("ec")`-style generated constants or split the EC into its own ACPI node under the LPC structural node.

## Migration path for the current Intel boards

### Pineview + ICH7 / Foxconn D41S

1. Revert/remove board-level `acpi_parent` and the topology-level `acpi_name: "PCI0"` workaround.
2. Make the Pineview runtime node the PCI root bridge ACPI node:
   - Set Pineview driver config `acpi_name: "PCI0"` (semantics: root bridge name, not MCHC name).
   - Refactor Pineview AML to match GM965 shape: `Device(PCI0) { ... Device(MCHC) { ... } Device(PDRC) { ... } _CRS/_OSC ... }`.
   - If the MCHC name should be configurable, add a separate `mchc_acpi_name` field or derive it from a structural child; do not overload `acpi_name`.
3. Nest the ICH7 aggregate under the northbridge/root bus in RON rather than keeping it as a top-level sibling with `acpi_parent`.
4. Mark ICH7 as `ParentScopeFragment` in the driver registry. Its own topology node can have no ACPI name.
5. Add ACPI names to structural child nodes where they represent namespace anchors:
   - LPC bus/function: `kind: Structural(LpcBus)`, `bus: Pci(0x1f, 0)`, `acpi_name: "LPCB"`.
   - SMBus: `kind: Structural(SmBus)`, `bus: Pci(0x1f, 3)`, `acpi_name: "SBUS"`.
   - PCIe root ports: `kind: Structural(PciBridge)`, `bus: Pci(0x1c, n)`, `acpi_name: "RP0{n+1}"` where useful for descendants.
6. Refactor ICH7 AML:
   - remove `Scope("\\_SB_.PCI0")`;
   - emit RCRB and PCI-function devices relative to `ctx.parent_scope` (`\_SB_.PCI0` for D41S);
   - obtain `LPCB`, `SBUS`, and root-port names from structural child context;
   - use root fragments for `_PIC` and sleep states.
7. SuperIO under the LPC structural node should naturally get contribution scope `\_SB_.PCI0.LPCB`; remove `SIO0` as a fake inclusion marker unless the driver actually emits `Device(SIO0)`.

### GM965 + ICH8 / Lenovo X61

1. GM965 already emits `Device(PCI0)`; keep that, but remove topology-level duplicate `acpi_name: "PCI0"` if driver config already names the root bridge.
2. Nest ICH8 under the GM965/root bus, remove `acpi_parent`.
3. Mark ICH8 as `ParentScopeFragment`; its contribution scope should derive as `\_SB_.PCI0`.
4. Add structural child ACPI names for `LPCB`, `SBUS`, and root ports as above.
5. Refactor ICH8 AML to use context-provided structural names and to stop documenting/hardcoding `\_SB.PCI0`.
6. Refactor GM965 root objects (`PICM`, `_PIC`, sleep states, CPU objects if intended under root/`_SB`) so placement is explicit. Root objects use `RootScope`; CPU devices should probably be under `\_SB_` unless coreboot reference requires otherwise.
7. Refactor `fstart-mainboard-lenovo-x61`:
   - Put the EC/dock ACPI node under the LPC structural path via topology-derived scope, not `Scope("\\_SB_.PCI0.LPCB")`.
   - Use generated path constants for methods/notifies that must reference the EC from root or `\_GPE`.
   - Consider adding an explicit `ec` structural/ACPI-only node under LPC with `acpi_name: "EC__"` so the mainboard driver can reference `ctx.path_by_device_name("ec")`.

## Validation strategy

1. Loader/schema tests:
   - RON with `acpi_parent` should fail once the field is removed (`deny_unknown_fields`).
   - Structural `acpi_name` must validate as a 4-char ACPI NameSeg.
   - Duplicate ACPI sibling names under the same derived parent should be rejected.
2. Namespace model unit tests using real boards:
   - Foxconn: root bridge path `\_SB_.PCI0`; ICH7 contribution scope `\_SB_.PCI0`; LPC path `\_SB_.PCI0.LPCB`; SuperIO contribution scope `\_SB_.PCI0.LPCB`.
   - Lenovo: root bridge path `\_SB_.PCI0`; ICH8 contribution scope `\_SB_.PCI0`; LPC path `\_SB_.PCI0.LPCB`; EC/mainboard fragments placed without hardcoded parent strings.
3. Codegen snapshot/source tests:
   - Generated ACPI prepare code should not contain board-derived `acpi_parent` lookup logic.
   - Generated scoped fragments should use paths produced by the namespace model.
4. Static grep checks:
   - No `acpi_parent` in `boards/**/*.ron` or schema.
   - No driver-owned `Scope("\\_SB_.PCI0")` for ICH7/ICH8/mainboard placement.
5. AML/table tests:
   - Update `crates/fstart-codegen/tests/acpi_dump.rs` to use the real board loader/codegen path, not manual Pineview+ICH7 concatenation.
   - Build DSDT, run `iasl -d` / `iasl -tc` via `nix-shell -p acpica-tools`, and assert expected scopes exist once: `\_SB_.PCI0`, `\_SB_.PCI0.LPCB`, `\_SB_.PCI0.LPCB.COM1`/EC as appropriate.
   - Verify checksums as today.
6. Build checks:
   - Host: `cargo test --workspace --exclude fstart-stage --exclude fstart-runtime --exclude fstart-alloc --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 --exclude fstart-platform-armv7`.
   - Firmware build-only for affected boards; for Foxconn D41S firmware images use release builds per project preference: `cargo xtask build --board foxconn-d41s --release`; also build `lenovo-x61` once migration touches it.

## Key decision points

- Do not accept per-board `acpi_parent`; it is a string reference workaround and conflicts with nested topology.
- Do accept ACPI names on structural topology nodes, because descendants need namespace anchors (`LPCB`, `SBUS`, `RPxx`) even when those nodes are not runtime drivers.
- Add driver contribution metadata/context so aggregate drivers can emit ACPI without pretending their runtime node has a single ACPI name.
- Treat hardcoded `PCI0` parent paths in drivers as bugs. Hardcoded standard child names may be temporary compatibility defaults, but clean ICH7/ICH8 migration should source `LPCB`/`SBUS`/`RPxx` from structural nodes or a driver ACPI context.
