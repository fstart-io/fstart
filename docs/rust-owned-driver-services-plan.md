# Rust-owned Services and Static-Codegen Plan

## Target architecture

The architecture is **data-only codegen with handwritten Rust codeflow**.

- Rust driver crates and `fstart-device-registry` own driver-provided service
  metadata.
- Board RON owns board wiring, stage policy, and typed per-instance policy.
- `fstart-codegen` emits static facts plus the minimum typed glue Rust requires
  for concrete board types.
- Handwritten Rust owns stage execution, capability sequencing, boot-media state
  machines, MP/SMM, payload loading, table preparation, and error/halt policy.

## Current implementation status

The stage entry path now follows the target shape:

- `fstart-codegen` emits data-only `STAGE_PLAN` statics.
- Generated `fstart_main()` only deserializes handoff, constructs
  `_BoardDevices`, and calls:

  ```rust
  fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN)
  ```

- `fstart-stage-runtime` owns `StagePlan`, feature-gated `StageOp` variants,
  and the handwritten executor.
- Flow arms are Cargo-feature gated so stages compile only the arms they use,
  including lifecycle/init arms (`stage-flow-clock-init`,
  `stage-flow-console-init`, `stage-flow-dram-init`, `stage-flow-driver-init`,
  `stage-flow-phases`, etc.) and optional families (`stage-flow-boot-media`,
  `stage-flow-ffs`, `stage-flow-fdt`, `stage-flow-mp`, `stage-flow-acpi`,
  `stage-flow-smbios`, `stage-flow-fel`).
- `xtask` derives those flow features from each stage's capability list.
- `DriverInit` policy now lives in the handwritten executor: codegen emits
  device ID tables, optional-device tables, and boot-media gated candidate
  tables; generated board code only provides `init_device(id)`.

The remaining transition is to narrow `Board`: the executor currently calls
high-level `Board` methods for several capability families. Those methods must
be replaced with primitive service/state accessors so runtime flow lives in
handwritten Rust modules, not generated method bodies.

## Strict generated-code boundary

### Codegen may emit

- concrete driver config literals;
- `_BoardDevices` fields for concrete driver types;
- static descriptor/config data;
- `DeviceId` constants and `DeviceId` arrays;
- static `StagePlan` / operation tables as declarative facts;
- static device topology, service, boot-media, payload, ACPI, SMBIOS, FDT, MP,
  microcode, and SMM descriptor tables;
- pure `DeviceId -> concrete field/ref/config` dispatch;
- construction of one concrete device from one static config;
- the small `fstart_main()` wrapper described above.

### Codegen must not emit

Generated code must not contain runtime algorithms or policy flow, including:

- loops over stage operations or phase participants;
- capability sequencing;
- calls to `fstart_mp::mp_init`;
- payload, UEFI, BL31, FIT, FDT, stage-load, next-stage, FEL, ACPI, or SMBIOS
  orchestration;
- boot-media matching, fallback ordering, or state-machine transitions;
- FFS anchor parsing or FFS file lookup;
- CPU-model dispatch, SMM image selection, or microcode lookup policy;
- logger installation policy beyond the primitive operation that installs a
  logger for an already-selected console `DeviceId`;
- root-first traversal, phase loops, skip/gated init policy, or halt-on-error
  decisions;
- generated function-pointer flow trampolines.

## `StagePlan` rules

`StagePlan` is data, not generated behavior. It may encode the ordered facts
selected by board RON:

```rust
pub struct StagePlan {
    pub stage_name: &'static str,
    pub ops: &'static [StageOp],
    pub persistent_inited: &'static [DeviceId],
    pub all_devices: &'static [DeviceId],
    pub optional_devices: &'static [DeviceId],
    pub boot_media_gated: &'static [BootMediaCandidate],
}
```

`StageOp` may name requested operations and reference static plan data. The
handwritten executor owns all `match StageOp` behavior.

Acceptable:

```rust
StageOp::ConsoleInit(UART0)
StageOp::BootMediaPlatformBootSource { plan: &BOOT_MEDIA_PLAN, .. }
StageOp::PayloadLoad
```

Not acceptable:

```rust
StageOp::GeneratedThunk(fn(&mut _BoardDevices) -> !)
StageOp::BootMediaAlreadyMatched { selected: MMC0 }
```

## Primitive board API target

The final board boundary must be primitive. The executor and capability modules
should express the flow in handwritten Rust using only these kinds of
operations:

- construct/init a device by `DeviceId`;
- borrow a concrete service implementation by `DeviceId` for the duration of a
  closure;
- read static descriptors and plan tables;
- read/write small board runtime state such as boot media, handoff metadata,
  detected memory map, prepared-table regions, and MP handle state;
- perform final platform primitives such as halt and jump.

High-level capability methods are not part of the final `Board` trait. Methods
to remove include:

- `memory_init`, `sig_verify`, `fdt_prepare`, `payload_load`, `stage_load`;
- `acpi_prepare`, `smbios_prepare`, `acpi_load` orchestration;
- `mp_init`;
- phase trampolines such as `pre_console_init`, `early_init`, etc.;
- `boot_media_platform_firmware_image` and `load_next_stage`.

`boot_media_select` has already been reduced to the primitive
`soc_boot_media()`: the executor now owns boot-source candidate matching and
boot-media state publication. Provider-backed `boot_media_firmware_image` has
been reduced to the primitive `firmware_image(provider)` plus executor-owned
state publication.

A likely primitive shape is closure-based service borrowing, avoiding `alloc`
while allowing handwritten runtime code to stay generic:

```rust
pub trait Board: Sized {
    fn init_device(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    fn with_console<R, F>(&mut self, id: DeviceId, f: F) -> Result<R, RuntimeError>
    where
        F: FnOnce(&dyn fstart_services::Console) -> R;

    fn with_block_device<R, F>(&mut self, id: DeviceId, f: F) -> Result<R, RuntimeError>
    where
        F: FnOnce(&mut dyn fstart_services::BlockDevice) -> R;

    fn with_firmware_image_provider<R, F>(
        &mut self,
        id: DeviceId,
        f: F,
    ) -> Result<R, RuntimeError>
    where
        F: FnOnce(&dyn fstart_services::FirmwareImageProvider) -> R;

    fn soc_boot_media(&self) -> Option<u8>;
    fn read_next_stage_window(&self, plan: &SocBootPlan) -> Result<NextStageWindow, RuntimeError>;

    fn boot_media_state(&self) -> &BootMediaState;
    fn set_boot_media_state(&mut self, state: BootMediaState);

    fn install_logger_on_console(&mut self, id: DeviceId) -> Result<(), RuntimeError>;

    fn halt(&self) -> !;
    fn jump_to(&self, entry: u64) -> !;
    fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> !;
}
```

Additional service-borrowing methods should be added only when a handwritten
executor/capability handler needs that service. If code size proves trait-object
calls are unacceptable for a hot path, use a primitive service-operation
forwarder such as `block_read(id, offset, buf)`, not a capability-level method
such as `payload_load()`.

## Migration phases from the current tree

### Phase 1 — generated-source invariants

Add source and API-shape tests that enforce:

- generated `fstart_main` only constructs handoff/board state and calls
  `run_stage`;
- generated source contains `STAGE_PLAN` data but no stage-flow loops;
- board-gen does not call `fstart_mp::mp_init` or perform payload/stage-load,
  boot-media matching, FFS parsing, table orchestration, or CPU dispatch;
- generated `impl Board` does not regain high-level capability methods after
  they are removed.

### Phase 2 — move lifecycle and phases into handwritten Rust

Move these algorithms out of generated board methods:

- root-first parent traversal;
- skip/gated init policy;
- phase loops over device IDs;
- logger install policy;
- DRAM/PCI init dispatch policy;
- flash-layout verification phase policy.

Codegen may retain per-device construction/access glue because concrete driver
config/type pairs are board-specific.

### Phase 3 — move boot-media flow into handwritten Rust

Handwritten Rust should own:

- provider/Rust-platform/block candidates;
- boot-source matching;
- fallback ordering;
- `BootMediaState` transitions;
- FFS anchor context publication.

Codegen should emit only candidates, provider IDs, image descriptors, scratch
buffer descriptors, and primitive accessors.

### Phase 4 — move payload/FIT/UEFI/BL31/FDT/stage-load/FEL flow

Move runtime logic out of generated methods for:

- FIT/buildtime/runtime payload loading;
- BL31 loading;
- UEFI launch and memory-map construction;
- FDT preparation/loading;
- stage-load/next-stage/FEL transitions.

Codegen should emit only static plans, addresses, candidate tables, and minimal
accessors.

### Phase 5 — move MP/SMM flow into handwritten Rust

Handwritten Rust should own:

- CPU kind dispatch;
- SMM image selection;
- SMM provider lookup/use through primitive accessors;
- microcode anchor lookup;
- `fstart_mp::mp_init` call and error handling.

Codegen should emit only `MpPlan` data and the SMM-provider `DeviceId`.

### Phase 6 — ACPI/SMBIOS data-only emission

Keep ACPI/SMBIOS codegen limited to static descriptors. Handwritten capability
code should perform preparation/load orchestration, buffer management, table
region tracking, and error policy.

### Phase 7 — docs, invariants, and size checks

Exit criteria:

- generated source contains data, concrete device fields/configs, primitive
  dispatch/access glue, and the small `fstart_main` wrapper only;
- no generated high-level capability methods exist;
- invariant tests prevent reintroducing generated runtime algorithms;
- representative host tests and board builds pass;
- D41S UEFI release size comparison is updated after the final refactor.

## Current generated-codeflow hotspots to eliminate

- `crates/fstart-codegen/src/stage_gen/board_gen/board_impl.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/mp.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/lifecycle.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/boot_media.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/payload.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/payload_uefi.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/fdt.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/platform/sunxi.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/caps_tables.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/phases.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/init_caps.rs`
- `crates/fstart-codegen/src/stage_gen/board_gen/logger.rs`

## Non-goals

- Do not restore board-owned service lists.
- Do not restore stringly typed services.
- Do not put structural topology back into `Service`.
- Do not put ACPI-only descriptors back into runtime devices.
- Do not require runtime driver discovery.
- Do not eliminate all generated code: concrete device fields, config literals,
  static plans, and primitive typed access glue are still expected.
