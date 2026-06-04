# Rust-owned Services and Static-Codegen Plan

## Actual target

The final architecture is **static-information-only codegen**:

- Rust driver crates and `fstart-device-registry` own all driver-provided service metadata.
- Board RON owns board wiring, stage policy, and typed per-instance policy only.
- `fstart-codegen` emits static facts and the minimum typed glue Rust requires.
- Handwritten Rust owns stage execution, capability sequencing, boot-media handling,
  MP/SMM, payload loading, table preparation, and error/halt policy.

## What codegen may emit

Allowed generated output:

- typed driver config literals and static descriptor data;
- `_BoardDevices` fields for concrete driver types;
- static `StagePlan` / operation tables;
- static device topology, service, boot-media, payload, ACPI, SMBIOS, FDT, and
  MP descriptor tables;
- small `DeviceId -> concrete field` accessors/thunks where Rust's type system
  requires board-specific concrete types;
- static arrays of device IDs for phases/capabilities.

## What codegen must not emit

Generated code must not contain runtime algorithms or policy flow such as:

- stage/capability sequencing;
- MP/SMM CPU-model dispatch, microcode lookup, or `fstart_mp::mp_init` setup;
- boot-media state machines, FFS anchor parsing, or block/media matching;
- payload, UEFI, BL31, FDT, stage-load, next-stage, or FEL control flow;
- ACPI/SMBIOS preparation/load orchestration;
- phase execution loops and error/halt policy;
- generated capability trampoline bodies beyond static data lookup or typed access.

## Completed in the current stack

Service ownership and schema cleanup are mostly done:

- `Service` / `ServiceSet` are typed registry metadata.
- `DriverInstance::provided_services()` computes effective service sets.
- Board RON no longer declares board-owned service lists.
- `disabled_services: [Service]` remains as typed per-instance board policy.
- Structural topology is separate from runtime services.
- Runtime SMBus is `Service::SystemManagementBus`, distinct from structural SMBus.
- ACPI-only descriptors use `kind: AcpiOnly, acpi: ...` and stay out of
  runtime device arrays and `DriverInstance`.
- Capability validation checks typed services before token emission.
- CK505 is enabled through typed SMBus child lifecycle plumbing.

This is **not** the full target. The current stack still has generated runtime
codeflow in `direct_flow.rs` and many `board_gen/*` modules.

## Remaining scope

### Phase 1 — inventory and guardrails

Classify each emitted token block as one of:

1. static data/config;
2. unavoidable typed device construction/access glue;
3. runtime codeflow to move.

Add failing invariants for the target architecture:

- generated `fstart_main` may only construct/load static data and call a
  handwritten executor;
- board-gen must not generate calls to `fstart_mp::mp_init`, payload/stage-load
  capability functions, CrabEFI launch internals, or boot-media matching logic;
- generated code must not contain CPU-model dispatch, FFS anchor parsing, or
  capability error/halt policy.

### Phase 2 — static stage-plan executor

Add handwritten runtime plan types, likely in `fstart-stage-runtime`:

- `StagePlan`
- `StageOp`
- `DevicePlan`
- `BootMediaPlan`
- `PayloadPlan`
- `FdtPlan`
- `StageLoadPlan`
- `MpPlan`

Then replace generated direct stage flow with:

```rust
static STAGE_PLAN: StagePlan = ...;

pub extern "C" fn fstart_main(handoff: usize) -> ! {
    let mut board = _BoardDevices::new(...);
    fstart_stage_runtime::run_stage(&mut board, &STAGE_PLAN, handoff)
}
```

### Phase 3 — narrow the `Board` trait

Shrink `Board` from capability trampolines to board-specific primitives:

- construct/init a device by `DeviceId`;
- access a device/service by `DeviceId` where needed;
- read/write board runtime state such as boot media and handoff metadata;
- expose static descriptor references.

Remove generated high-level methods such as `payload_load`, `stage_load`,
`fdt_prepare`, `mp_init`, and phase trampolines once the executor owns those
flows.

### Phase 4 — move MP/SMM flow to Rust

Move `board_gen/mp.rs` runtime logic into handwritten Rust:

- CPU kind dispatch;
- SMM image selection;
- SMM provider lookup/use through `DeviceId` accessor;
- microcode anchor lookup;
- `fstart_mp::mp_init` call and error handling.

Codegen should emit only `MpPlan` data and the SMM-provider `DeviceId`.

### Phase 5 — move boot-media, payload, UEFI, FDT, and stage-load flow

Move runtime logic out of:

- `board_gen/boot_media.rs`
- `board_gen/payload.rs`
- `board_gen/payload_uefi.rs`
- `board_gen/fdt.rs`
- `board_gen/platform/sunxi.rs`

Handwritten Rust should own:

- boot-media selection and matching;
- FFS anchor parsing;
- FIT/buildtime/runtime payload loading;
- BL31 loading;
- UEFI launch and memory-map construction;
- FDT preparation/loading;
- stage-load/next-stage/FEL transitions.

Codegen should emit only plans, static addresses, static candidate tables, and
minimal device accessors.

### Phase 6 — move lifecycle and phase algorithms to Rust

Move these algorithms out of generated code:

- root-first parent traversal;
- skip/gated init policy;
- phase loops over device IDs;
- logger install policy;
- DRAM/PCI init dispatch policy;
- flash-layout verification phase policy.

Codegen may still emit per-device construction thunks because concrete driver
config/type pairs are board-specific.

### Phase 7 — ACPI/SMBIOS data-only emission

Keep ACPI/SMBIOS codegen limited to static descriptors. Handwritten capability
code should perform preparation/load orchestration, buffer management, table
region tracking, and error policy.

### Phase 8 — docs and final invariants

Update docs once the executor/Board shape is real:

- `docs/driver-model.md`
- `docs/architecture.md`
- `docs/continuation-plan.md`
- this plan

Exit criteria:

- no docs claim direct generated codeflow as the target;
- invariant tests prevent reintroducing generated runtime algorithms;
- representative host tests and board builds pass;
- D41S UEFI release size comparison is updated after the final refactor.

## Current known generated-codeflow hotspots

- `crates/fstart-codegen/src/stage_gen/direct_flow.rs`
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
  static plans, and typed access thunks are still expected.
