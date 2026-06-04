# Rust-owned Driver Services Plan

## Summary

Board RON files used to repeat facts that are already properties of Rust
code. A device node could say `services: ["Console"]`, while the selected Rust
`DriverInstance` also had driver-owned service metadata. Codegen then used a mix
of board-declared services, driver metadata, string comparisons, and special
cases.

This plan removes that duplication completely.

After this work:

- **Rust driver crates / registry are the single source of truth for what a
  driver can provide**: `Console`, `BlockDevice`, `MemoryController`,
  `PciRootBus`, ACPI contribution, bus-device construction, etc.
- **Board RON files describe board wiring and policy only**: which device
  instances exist, how they are wired, what config values they use, which
  stage capabilities select which named devices, and any per-device policy such
  as suppressing one Rust-provided service on one instance.
- **No legacy `services: [...]` field remains in board RON or in
  `DeviceConfig`.**
- **Codegen uses typed enums and precomputed models**, not ad hoc string
  comparisons.

This is intentionally ambitious. There is no compatibility mode and no long-term
fallback path. Existing boards are updated in the same change series.

## Problem statement

Today, device service information is encoded in at least two places:

1. Board RON:

   ```ron
   ( name: "uart0", driver: Ns16550(( ... )), services: ["Console"] )
   ```

2. Rust registry metadata:

   ```rust
   Self::Ns16550(_) => &DriverMeta {
       services: &["Console"],
       ...
   }
   ```

This causes several issues:

- A board can lie about what a driver implements.
- Codegen has to ask both `dev.services` and `inst.meta().services` depending on
  the path.
- Service names are stringly typed throughout `board_gen.rs`.
- Structural topology tags like `"LpcBus"`, `"PciBridge"`, and `"SmBus"` are
  mixed with real Rust service traits.
- Config-dependent services, such as SuperIO console support, cannot be modeled
  cleanly with only static metadata.
- `board_gen.rs` becomes a large pile of special-case scans over RON strings.

## Design principle

Separate these concepts:

| Concept                                    | Source of truth                              | Examples                                                                   |
| ------------------------------------------ | -------------------------------------------- | -------------------------------------------------------------------------- |
| Driver capability / service implementation | Rust driver crate + registry                 | `Ns16550 provides Console`, `QemuFwCfg provides MemoryDetector`            |
| Board wiring                               | Board RON                                    | MMIO base, bus parent, PCI slot/function, I2C address                      |
| Board policy / stage sequencing            | Board RON                                    | `ConsoleInit(device: "uart0")`, `PayloadLoad`, `DramInit(device: "dram0")` |
| Structural topology                        | Board RON typed node kind                    | LPC bus node, PCIe port node, SMBus branch                                 |
| Optional/config-dependent services         | Rust registry method inspecting typed config | SuperIO provides `Console` only when `console_port` is configured          |

A board may choose **which instance** fulfils a role, and may suppress a
specific service for a specific instance with typed policy. It may not declare
**what interfaces the driver type implements**.

## Target RON shape

### Runtime device

Before:

```ron
(
    name: "uart0",
    driver: Ns16550((
        base_addr: 0x10000000,
        clock_freq: 3686400,
        baud_rate: 115200,
    )),
    services: ["Console"],
)
```

After:

```ron
(
    name: "uart0",
    driver: Ns16550((
        base_addr: 0x10000000,
        clock_freq: 3686400,
        baud_rate: 115200,
    )),
)
```

`Console` comes from the `Ns16550` driver metadata.

Per-device policy can subtract one of the Rust-provided services without
removing the device itself:

```ron
(
    name: "uart0",
    disabled_services: [Console],
    driver: Ns16550((
        base_addr: 0x10000000,
        clock_freq: 3686400,
        baud_rate: 115200,
    )),
)
```

This keeps the UART device present but prevents generated role discovery from
selecting it as a `Console` provider. The field is typed (`[Console]`), not a
legacy string list (`["Console"]`), and codegen validates that the selected
driver actually provides every disabled service.

### Structural node

Before:

```ron
(
    name: "lpc",
    services: ["LpcBus"],
    children: [ ... ],
)
```

After:

```ron
(
    name: "lpc",
    kind: Structural(LpcBus),
    children: [ ... ],
)
```

Structural nodes are topology only. They are not runtime devices and they do not
provide Rust service traits.

### ACPI-only descriptor

Before, ACPI-only entries are represented as pseudo-driver variants and often
carry empty services:

```ron
(
    name: "xhci0",
    driver: Xhci(( ... )),
    services: [],
)
```

After, keep them as explicit non-runtime descriptors, not devices that pretend to
have no services:

```ron
(
    name: "xhci0",
    kind: AcpiOnly,
    acpi: Xhci(( ... )),
)
```

ACPI-only descriptors are parsed into a side table outside the runtime device
arrays. They are not `DriverInstance` variants and must not use the runtime
`driver:` field.

## New typed model

### `Service`

Add a typed service enum in a host-visible crate. Recommended location:
`fstart-device-registry`, because this is registry/codegen metadata and should not
force target firmware to carry extra schema machinery.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    Console,
    BlockDevice,
    ClockController,
    MemoryController,
    PciRootBus,
    PciHost,
    SmmOps,
    Framebuffer,
    AcpiTableProvider,
    MemoryDetector,
    SuperIoHost,
    Southbridge,
    Mainboard,
    PreConsoleInit,
    EarlyInit,
    StageLocalInit,
    PostDramInit,
    FinalizeInit,
    FlashLayoutVerifier,
    FirmwareImageProvider,
    I2cBus,
    SpiBus,
    GpioController,
    /// Runtime SMBus trait provider; distinct from StructuralKind::SmBus.
    SystemManagementBus,
}
```

Do not expose arbitrary string services. Adding a new service should require
adding a new enum variant and updating the registry mapping.

### `ServiceSet`

Use a small no-allocation representation for codegen convenience:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSet(u128);
```

The important part is that callers use:

```rust
inst.provides(Service::Console)
```

not:

```rust
dev.services.iter().any(|s| s.as_str() == "Console")
```

### `DriverInstance::provided_services()`

Static metadata is not enough for every driver. Add an instance method that can
inspect typed config:

```rust
impl DriverInstance {
    pub fn provided_services(&self) -> ServiceSet;

    pub fn provides(&self, service: Service) -> bool {
        self.provided_services().contains(service)
    }
}
```

Examples:

```rust
Self::Ns16550(_) => services![Service::Console]
Self::SunxiMmc(_) => services![Service::BlockDevice]
Self::QemuFwCfg(_) => services![Service::AcpiTableProvider, Service::MemoryDetector]
```

Config-dependent example:

```rust
Self::Ite8721f(cfg) => {
    let mut services = ServiceSet::new();
    services.insert(Service::SuperIoHost);
    if cfg.console_port.is_some() {
        services.insert(Service::Console);
    }
    services
}
```

### `DriverMeta`

Change `DriverMeta` from string services to typed/static properties:

```rust
pub struct DriverMeta {
    pub name: &'static str,
    pub type_name: &'static str,
    pub module_path: &'static str,
    pub config_type: &'static str,
    pub static_services: &'static [Service],
    pub compatible: &'static [&'static str],
    pub has_acpi: bool,
    pub construction: ConstructionKind,
}

pub enum ConstructionKind {
    Device,
    BusDevice,
    Structural,
}
```

ACPI-only descriptors are deliberately outside `DriverInstance`, so they do not
have a `ConstructionKind`.

`static_services` is for unconditional services. Codegen should prefer
`DriverInstance::provided_services()` when deciding what an instance provides.

## RON schema changes

### Remove `DeviceConfig.services`

Delete this field:

```rust
pub services: heapless::Vec<HString<32>, 8>,
```

from `fstart_types::device::DeviceConfig`.

This is a breaking schema change. All boards must be updated in the same series.

### Introduce explicit node kind

Add a typed node kind for non-runtime topology entries:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceNodeKind {
    Device,
    Structural(StructuralKind),
    AcpiOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuralKind {
    PciBridge,
    LpcBus,
    SmBus,
    I2cMux,
    SpiChipSelect,
    GenericBus,
}
```

Possible final `DeviceConfig` shape:

```rust
pub struct DeviceConfig {
    pub name: HString<32>,
    pub kind: DeviceNodeKind,
    pub driver: HString<32>,
    pub parent: Option<HString<32>>,
    pub bus: Option<BusAddress>,
    pub enabled: bool,
}
```

For normal runtime devices, `kind` may default to `Device` in serde.

For structural nodes, `driver` should not use the legacy `"_structural"`
sentinel. Either make `driver` optional or split parsed RON from flattened
`DeviceConfig`:

```rust
pub driver: Option<HString<32>>
```

Preferred final state: **no sentinel driver names**.

### Parser changes

`ron_loader` currently has an internal `RawDeviceConfig` with `services` and
`driver: Option<DriverInstance>`. Replace it with:

```rust
struct RawDeviceConfig {
    name: HString<32>,
    kind: DeviceNodeKind,
    parent: Option<HString<32>>,
    bus: Option<BusAddress>,
    enabled: bool,
    driver: Option<DriverInstance>,
    children: heapless::Vec<RawDeviceConfig, N>,
}
```

Validation rules:

- `kind = Device` requires `driver = Some(...)`.
- `kind = Structural(_)` requires `driver = None`.
- `kind = AcpiOnly` requires an `acpi: ...` descriptor and no runtime `driver`.
- A runtime driver may not be used with `kind = Structural`.
- A structural node may have children but is never materialized as a runtime
  field.

## Codegen model changes

### Add `BoardEmitModel`

Before emitting tokens, build a typed semantic model once:

```rust
pub struct BoardEmitModel<'a> {
    pub config: &'a BoardConfig,
    pub stage: StageScope<'a>,
    pub devices: RuntimeDeviceTable<'a>,
    pub topology: TopologyModel<'a>,
}
```

### Add `RuntimeDevice`

```rust
pub struct RuntimeDevice<'a> {
    pub id: DeviceId,
    pub name: &'a str,
    pub field: Ident,
    pub instance: &'a DriverInstance,
    pub services: ServiceSet,
    pub construction: ConstructionKind,
    pub parent: Option<DeviceId>,
    pub real_parent: Option<DeviceId>,
    pub init_chain: heapless::Vec<DeviceId, MAX_DEPTH>,
}
```

All device queries should go through this table:

```rust
model.devices.providers(Service::Console)
model.devices.providers(Service::BlockDevice)
model.devices.get(id).provides(Service::PciRootBus)
```

### Add `StageScope`

Centralize stage-level facts that are currently recomputed in many emitters:

```rust
pub struct StageScope<'a> {
    pub name: Option<&'a str>,
    pub capabilities: &'a [Capability],
    pub is_first_stage: bool,
    pub uses_ffs: bool,
    pub uses_fdt: bool,
    pub uses_acpi: bool,
    pub uses_smbios: bool,
    pub uses_sunxi_egon: bool,
    pub uses_smm: bool,
}
```

Emitters should not inspect raw capability slices unless they are lowering that
specific capability.

### Eliminate string service checks

Replace every pattern like:

```rust
ctx.devices[idx].services.iter().any(|s| s.as_str() == "Console")
```

with:

```rust
model.devices[idx].provides(Service::Console)
```

Replace `phase_init_body(ctx, "PreConsoleInit", "PreConsoleInit", "pre_console_init")`
with typed specs:

```rust
struct PhaseSpec {
    service: Service,
    trait_tokens: TokenStream,
    method: Ident,
    capability_kind: CapabilityKind,
}
```

## Board adapter cleanup

Once the typed model exists, split `board_gen.rs` into smaller modules. Target
layout:

```text
crates/fstart-codegen/src/stage_gen/board_gen/
  mod.rs              # orchestration only
  model.rs            # BoardEmitModel, StageScope, RuntimeDeviceTable
  state.rs            # _BoardDevices struct and new()
  lifecycle.rs        # init_device, init_all_devices
  boot_media.rs       # BootMediaState matching helpers
  phases.rs           # PreConsole/Early/PostDram/etc.
  logger.rs           # install_logger
  caps/
    fdt.rs
    payload.rs
    stage_load.rs
    next_stage.rs
    acpi.rs
    smbios.rs
    mp.rs
  platform/
    x86.rs
    sunxi.rs
    linux.rs
```

`board_gen/mod.rs` should become mostly:

```rust
pub(super) fn generate_board_adapter(...) -> TokenStream {
    let model = BoardEmitModel::new(...);
    quote! {
        #state
        #impl_board
    }
}
```

## Validation changes

Add validation that stage capabilities reference devices that actually provide the
required service:

| Capability                          | Required service/property                                     |
| ----------------------------------- | ------------------------------------------------------------- |
| `ConsoleInit { device }`            | `Service::Console`                                            |
| `DramInit { device }`               | `Service::MemoryController`                                   |
| `PciInit { device }`                | `Service::PciRootBus` or final chosen PCI service name        |
| `AcpiLoad { device }`               | `Service::AcpiTableProvider`                                  |
| `MemoryDetect { device }`           | `Service::MemoryDetector`                                     |
| `BootMedia(FirmwareImage { provider })` | explicit/effective `Service::FirmwareImageProvider`, Rust platform mapping, or Rust platform boot-source candidates |
| `LoadNextStage { devices, .. }`     | each candidate has block/media service and boot-media mapping |
| phase init capabilities             | each named device provides the phase service                  |
| `MpInit { smm_provider }`           | provider supplies `Service::SmmOps`                           |

This validation should happen before token emission and should produce a clear
build error, not a generated `todo!()` body.

## Generated imports

`generate_driver_imports` currently also scans RON service strings to decide which
service traits to import. Replace this with the typed model:

- Import driver crates/types from runtime devices.
- Import service traits from `RuntimeDevice.services` and `StageScope`.
- Import FFS/boot-media types from `StageScope.uses_ffs` and selected boot-media
  kinds.
- Import ACPI/SMBIOS/CrabEFI/FDT crates from `StageScope` and payload kind.

No import decision should depend on a RON `services` field.

## Tests

### Schema tests

- Every board RON loads without `services`.
- A board that includes `services` fails with a useful parse error.
- Structural nodes use `kind: Structural(...)`, not `_structural`.
- Runtime device without `driver` fails.
- Structural node with `driver` fails.

### Registry and driver tests

- Each real driver reports expected services through `provided_services()`.
- Config-dependent service cases are covered:
  - SuperIO with `console_port` provides `Console`.
  - SuperIO without `console_port` does not provide `Console`.
- Runtime SMBus providers report `Service::SystemManagementBus`.
- CK505 validates that the RON bus address is `BusAddress::I2c` and that masked
  SMBus register writes preserve bits outside each mask.
- ACPI-only descriptors stay outside `DriverInstance` entirely.
- Structural nodes report `ConstructionKind::Structural` and no services.

### Codegen tests

- Generated board adapter contains no service-string matching.
- `board_gen.rs` contains no `"Console"`, `"BlockDevice"`, etc. comparisons.
  String literals may remain only for log messages and generated Rust paths.
- Console selection uses `Service::Console`.
- Block-device boot media uses `Service::BlockDevice`.
- ACPI load uses `Service::AcpiTableProvider`.
- Memory detect uses `Service::MemoryDetector`.
- Phase init uses typed `PhaseSpec`.

### Whole-board tests

Run existing host tests:

```bash
cargo test --workspace --exclude fstart-stage --exclude fstart-runtime \
    --exclude fstart-alloc \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 \
    --exclude fstart-platform-armv7
```

Run representative board builds:

```bash
cargo xtask build --board qemu-riscv64
cargo xtask build --board qemu-aarch64
cargo xtask build --board qemu-armv7
cargo xtask build --board qemu-q35
cargo xtask build --board qemu-sbsa
cargo xtask build --board foxconn-d41s --release
cargo xtask build --board foxconn-d41s-uefi --release
```

## Implementation phases

Status: phases 1–8 are implemented. The plan remains as design history and as
an invariant checklist for future driver/service work. Configuration errors that
affect reachable stage plans should fail before token emission with
`compile_error!` diagnostics. Generated dead-code `Board` trait methods use
explicit `unreachable!()` bodies instead of `todo!()` stubs.

### Phase 1 — typed services in registry

1. Add `Service` and `ServiceSet` to `fstart-device-registry`.
2. Change `DriverMeta.services` to `DriverMeta.static_services: &'static [Service]`.
3. Add `DriverInstance::provided_services()` and `DriverInstance::provides()`.
4. During this phase only, keep `DeviceConfig.services` unused until the
   schema-removal phase deletes it.
5. Add registry unit tests.

Exit criteria:

- All driver metadata uses typed services.
- No new code introduces string service names.

### Phase 2 — semantic model for codegen

1. Add `BoardEmitModel`, `StageScope`, and `RuntimeDeviceTable`.
2. Populate runtime-device services from `DriverInstance::provided_services()`.
3. Move `enabled_indices`, parent-chain walking, real-parent resolution, and
   exclusion logic into the model.
4. Update `board_gen.rs` to query the model for at least:
   - logger
   - boot-media block device arms
   - DRAM init
   - PCI init
   - ACPI load
   - memory detect
   - phase init

Exit criteria:

- `board_gen.rs` no longer reads `DeviceConfig.services`.

### Phase 3 — validation from typed services

1. Update capability validation to check required services through
   `DriverInstance::provides()`.
2. Reject capabilities that name a device lacking the required service.
3. Reject stage plans that require FFS/ACPI/SMBIOS/FDT without the proper stage
   scope.
4. Replace dead-code `todo!()` bodies with earlier validation or small
   `unreachable!()` bodies that do not hide configuration errors.

Exit criteria:

- Incorrect board role assignment fails during validation with a clear message.

### Phase 4 — remove RON `services`

1. Delete `services` from `RawDeviceConfig`.
2. Delete `services` from `DeviceConfig`.
3. Update every board RON file to remove `services: [...]`.
4. Introduce `kind: Structural(...)` for structural nodes.
5. Remove the `"_structural"` sentinel.
6. Update docs and examples.

Exit criteria:

- `rg "services:" boards crates/fstart-types crates/fstart-codegen/src/ron_loader.rs`
  finds no schema usage.
- `rg "_structural"` finds no production usage.

### Phase 5 — ACPI-only cleanup

1. Use explicit `kind: AcpiOnly, acpi: ...` RON entries for ACPI-only
   descriptors.
2. Keep ACPI-only descriptors outside `DriverInstance` and the runtime device
   arrays entirely.
3. Make runtime device iteration only include actual runtime drivers and
   structural topology nodes.

Exit criteria:

- Runtime device model has no ACPI-only pseudo-devices.
- ACPI emitters consume ACPI descriptor models explicitly.

### Phase 6 — split `board_gen.rs`

1. Move tests out of `board_gen.rs`.
2. Split model, state, lifecycle, boot media, phases, logger, and capabilities
   into modules.
3. Move platform-specific code into `platform::{x86,sunxi,linux}` helpers.
4. Keep `board_gen/mod.rs` as a small orchestration module.

Exit criteria:

- No single `board_gen` module exceeds roughly 700 lines.
- Capability emitters are domain-local and use `BoardEmitModel` only.

### Phase 7 — final cleanup and invariants

1. Delete transitional comments that describe obsolete migration state.
2. Update `docs/driver-model.md` to say:
   - RON is source of truth for board wiring and stage policy.
   - Rust registry is source of truth for driver-provided services.
3. Add CI-like grep checks or tests preventing reintroduction of:
   - `DeviceConfig.services`
   - RON `services:`
   - service string comparisons in codegen
   - `_structural` sentinel driver name
4. Run format, tests, and representative builds.

Exit criteria:

- There is no compatibility layer or legacy schema left.
- All boards use the new schema.
- Codegen service dispatch is typed end-to-end.

### Phase 8 — bus-child lifecycle plumbing

1. Extend `BusDevice` with `new_on_bus_at(config, bus, Option<BusAddress>)` for
   devices whose address is board topology rather than driver config.
2. Extend `BusDevice` with `init_on_bus(&mut self, &mut Bus)` for children that
   need mutable parent-bus transactions during initialization.
3. Generate bus-child construction with the parsed RON `bus:` address and call
   `init_on_bus()` with a mutable parent reference after construction.
4. Keep runtime bus services typed: `Service::SystemManagementBus` maps to the
   target `SmBus` trait and remains separate from structural
   `StructuralKind::SmBus` topology.
5. Remove CK505's parent-address bridge and use `dyn SmBus` directly, with
   masked SMBus byte read/modify/write programming in `init_on_bus()`.

Exit criteria:

- `I2cCk505` no longer carries custom parent address-provider plumbing.
- Generated bus-child code passes `BusAddress` into `new_on_bus_at()`.
- Generated bus-child code can initialize children through mutable parent-bus
  transactions without aliasing an already-borrowed immutable parent reference.
- CK505 unit tests cover topology-address validation and masked SMBus writes.
- Foxconn D41S and Foxconn D41S UEFI release builds pass with CK505 enabled.

## Post-implementation cleanup status

No deferred cleanup items currently remain for this plan. AArch64 EL2/TF-A
relocate-entry policy is derived from the existing typed platform, SoC image
format, memory map, and first-stage load address rather than from a board name.
Foxconn D41S CK505 is enabled through the typed SMBus child lifecycle, SMM is
enabled in ramstage, ACPI CPU count is explicit in board policy, and SMBIOS
CPU/cache/DIMM fields are populated in board RON instead of relying on generated
sentinels. The unused `LateDriverInit` placeholder capability was removed;
boards should use typed phase capabilities such as `FinalizeInit` for real
lockdown/finalization work.

## Non-goals

- This plan does not introduce runtime dynamic dispatch.
- This plan does not make drivers discoverable at runtime.
- This plan does not remove board RON as the description of board wiring.
- This plan does not move stage sequencing out of RON.

## Expected impact

The largest payoff is not just removing `services: [...]` from RON. The bigger
win is that `board_gen.rs` stops being forced to rediscover board semantics from
raw strings. With a typed model, most emitters become mechanical:

```rust
for dev in model.devices.providers(Service::Console) {
    emit_console_arm(dev);
}
```

instead of scanning RON strings, checking driver metadata separately, and adding
case-by-case guards.

The end state should be easier to validate, harder for board files to get wrong,
and much easier to extend with new drivers and services.
