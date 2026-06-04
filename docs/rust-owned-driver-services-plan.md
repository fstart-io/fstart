# Rust-owned Driver Services Plan

## Status

Implemented. Rust driver crates and `fstart-device-registry` now own driver
service metadata. Board RON files describe board wiring, stage policy, and typed
per-instance policy only.

## Achieved model

- Driver service metadata is typed with `Service` and `ServiceSet` in
  `fstart-device-registry`.
- `DriverInstance::provided_services()` returns the effective service set for an
  instance and may inspect typed driver config.
- Board RON files no longer declare driver-provided service lists.
- `disabled_services: [Service]` remains as typed board policy that subtracts a
  Rust-provided service from one instance.
- Structural topology uses `kind: Structural(...)` and is kept separate from
  runtime service traits.
- Runtime SMBus providers use `Service::SystemManagementBus`; this is distinct
  from structural SMBus topology.
- ACPI-only descriptors use `kind: AcpiOnly, acpi: ...` and are stored outside
  runtime device arrays and outside `DriverInstance`.
- Codegen queries precomputed typed device models instead of comparing service
  names as strings.
- Reachable stage-plan misconfiguration is validated before token emission and
  reported with clear diagnostics.
- Generated dead-code `Board` trait methods use explicit `unreachable!()` bodies
  rather than placeholder implementation stubs.

## Board/codegen invariants

- Board RON may choose which named instance fulfils a role through stage
  capabilities such as `ConsoleInit`, `DramInit`, `PciInit`, `AcpiLoad`,
  `MemoryDetect`, `LoadNextStage`, and phase-init capabilities.
- Board RON may not define what service traits a driver implements.
- Runtime devices require a typed `driver:` variant.
- Structural nodes have no runtime driver and do not appear as initialized
  runtime devices.
- ACPI-only descriptors have no runtime driver and are emitted only through ACPI
  descriptor side tables.
- Capability validation checks typed services before code generation.
- Driver import generation is based on typed runtime-device services and stage
  scope.

## Validation coverage

The invariant tests cover:

- all board RON files parse with the current schema;
- board RON files do not declare a board-owned service-list field;
- `DeviceConfig` has no board-owned service-list field;
- structural topology markers are not encoded as production driver names;
- ACPI-only descriptors use `acpi:` and stay out of runtime device tables;
- ACPI-only descriptors are not `DriverInstance` variants;
- structural topology variants are not `Service` variants;
- codegen does not compare typed service names as string literals;
- generated production code has no placeholder stubs;
- board-gen modules stay under the size cap.

Representative validation performed for this plan included host tests,
codegen/schema invariant tests, QEMU board builds, SBSA build coverage, and
Foxconn D41S release builds.

## Foxconn D41S completion

- CK505 is enabled on both Foxconn D41S board configurations.
- Generated bus-child lifecycle code passes parsed `BusAddress` into
  `new_on_bus_at()` and performs mutable parent-bus child initialization with
  `init_on_bus()`.
- `I2cCk505` uses `dyn SmBus` directly and programs masked SMBus byte
  read/modify/write transactions.
- SMM is enabled in ramstage.
- ACPI CPU count is explicit in board policy.
- SMBIOS CPU/cache/DIMM descriptors are populated in board RON.

## Non-goals

- This plan does not introduce runtime dynamic dispatch.
- This plan does not make drivers discoverable at runtime.
- This plan does not remove board RON as the description of board wiring.
- This plan does not move stage sequencing out of RON.
