# Concrete platform build-plan boundary

This corrects the tooling boundary left by [Intel typed facts](architecture-intel-typed-facts.md).
That change moved hardware facts and feature policy into Rust, but fbuild still
selected Intel roles, SMM consumers, linker entry points and ELF checks. Merely
moving its Intel executor would not have fixed the boundary.

## Migrated scope

**X61 and QEMU AArch64 use the same concrete plan and executor.** This is not a
claim that all of fbuild is generic. RISC-V/ARMv7 QEMU metadata profiles use the
explicit `ResolvedImage::Legacy` route; remaining board-host/BoardConfig builds,
including i945 and Pineview, retain their earlier legacy path. There is no
AArch64 Cargo geometry recipe alongside its migrated Rust constructor.

- `crates/image-build/src/build_plan.rs`: family-free `BuildPlan`, named
  `CompilationUnit`s, explicit Cargo target kind/triple, entry/environment/payload
  cfg values, features, compiler flags/build-std, linker text, ELF expectations,
  generated-artifact bindings and assembly projection.
- `crates/platform-intel/src/host.rs`: platform-owned conversion into four units.
  The SMM library unit explicitly selects release/PIC compilation and its format
  operation. Ramstage explicitly binds that producer's current image/header.
  The executor does not discover that relationship from a role or name.
- `crates/platform-qemu/src/{facts,host}.rs` and
  `boards/qemu/aarch64/src/config.rs`: unconditional typed physical flash fact,
  fixed platform reservations and a single relocating compiler unit. Storage
  starts at zero; execution is at `0x40400000`; writable storage is separate at
  `0x40800000`. No producer, CAR flow or Intel entry convention is imposed.
- `tools/fbuild/src/{host_plan,plan_executor,selection}.rs`: existing Cargo-owned
  host runner, alias qualification, bounded dependency execution and one compiler
  selection used by build, check and IDE. The old `intel_build.rs` and
  `resolved_intel.rs` executors are removed.
- `crates/image-build/src/{linker,elf,intel_assembly}.rs`: reusable linker/ELF/image
  format mechanisms. Platforms select helpers and emit their concrete results;
  the common tools do not select a platform helper by family identity.

`UnitOutput::SmmImage` is an existing image-format operation, not a platform
identity: target, Cargo library selection, cfgs, flags, features and consuming
bindings are separate explicit fields. Its handler is linked from **only the
current Cargo JSON rlib artifacts**. Executables likewise come from Cargo JSON,
not guessed shared target paths. Build/check receipts are retained beside each
compiler selection. Whole-plan identity includes generated linker text and
compiler options; consuming directories additionally hash length-delimited
producer identities and actual file bytes.

## Bounded dependencies, not an authored workflow

The plan has at most 32 units. It rejects duplicate/invalid unit IDs, missing
producers/artifacts, duplicate environment bindings and cycles before launching
commands. Named selection takes only the selected unit's transitive producer
closure. Prerequisites are built even for check/IDE; the selected unit is then
built, checked or analyzed with the same compiler selection. Whole-image check
builds required producers and checks non-producers.

Unit names are safe path components. Environment names follow portable
identifier rules, reject compiler-flag overrides and NUL values, and are not
validated as filesystem names. Each platform emits an independent allowed cfg
schema; selected entry/environment/payload values must belong to it. Common
selection generates check-cfg flags from that schema, with no tooling-owned value
registry and no automatic whitelisting of the selected value. A real rustc test
accepts novel platform vocabulary under `-Dunexpected_cfgs` and rejects a typo.
The pre-resolution host runner selects none of these firmware cfgs: it registers
only their names with `values(any())`, so inactive new board guards compile
without a tooling vocabulary. An actual fresh/cached Cargo fixture uses novel
guards under `deny(unexpected_cfgs)` and asserts no firmware branch is selected;
the emitted target units still use strict independent schemas.

Every plan artifact binding key is removed from units that do not bind it. Direct
Cargo commands and metadata/audit use the same environment application helper;
IDE check commands encode equivalent `env -u` removals, while bound values
explicitly overwrite ambient ones. Preparing a common selection also rejects
unrecorded ambient `FSTART_*` firmware inputs, including producer keys from other
plans (`FSTART_WORKSPACE_ROOT` remains tool configuration). The editor graph
records compiler/build-script environments, not an inferred producer namespace.

Payload input bindings are the bounded existing kernel/firmware/FIT slots; unknown
or duplicate slots and zero capacities fail both at plan validation and at the
input-validation entry point. Unsupported required file kinds cannot be silently
ignored. There are no arbitrary commands,
board-authored DAGs, lifecycle steps or new RON/TOML authoring schemas.

```sh
fbuild build -b lenovo-x61 --release --payload halt --stage smm
fbuild check lenovo-x61 --release --payload halt --stage postcar
fbuild ide lenovo-x61 --release --payload uefi --stage ramstage
fbuild ide qemu-aarch64 --release --payload halt --stage stage
```

Stage names are strings resolved against the selected plan. No CLI Intel enum
or IDE ramstage/SMM special case remains. A multistage IDE view requires a named
unit; a one-stage image defaults to its sole image unit. Unknown names fail
rather than silently selecting a different stage.

## Assembly and remaining limits

`Assembly` is a projection into the existing assembler, **not a source of
compiler policy**. It carries memory/image placement, explicit IFD format data,
stage compression/load relationships, security, payload/microcode inputs,
SoC image format and boot-hart ID. Authenticated-bootstrap wire roles are explicit
bindings supplied by the platform: the common assembly path never infers
Postcar/Mainstage from a compiler unit name. Name inference exists only for
explicit legacy callers without these bindings.

The shim supplies no runtime CAR, ACPI, SMBIOS or SMM objects: those are not
assembler inputs and remain owned by the firmware/platform flow. It currently
projects raw memory-mapped and Intel-IFD images; this is not a universal flash
partition or SoC image framework. Adding genuinely new image/compiler mechanisms
can require library support. Adding a platform using the represented mechanisms
requires no new fbuild family enum, match arm or CLI branch. New mechanisms must
not be smuggled in as platform identity tests in the executor.

Existing register sequences, auth/stash/handoff ABI, reset relocation code and
runtime descriptor authority are unchanged. Intel's full BIOS mapping/MTRR
identity remains distinct from FFS capacity. ACPI remains normal ramstage policy,
independent of EC initialization. Fixed capacities do not adapt to linked sizes;
ELF validation checks architecture/class/endianness, PT_LOAD physical and virtual
bounds, entries, linear copy extents, symbols and exact retained descriptor bytes.
The descriptor's file bytes must actually be covered by its load mapping.

## Cargo/editor contract

The selected-source preparation and lock ownership are unchanged. The root lock
adds only local edges QEMU → image-build/serde_json and fbuild's test-only QEMU
host dependency; external versions/sources/checksums are unchanged. Board locks
are retained. The generated runner still appends one deterministic local package
entry to its prepared lock and builds with `--locked`, with no fallback. This
continues to be a guarantee **after** the pre-existing selected-source preparation,
not a claim of end-to-end zero-resolution builds.

The AArch64 board uses the same platform-owned hygienic stage export pattern as
Intel. Its physical fact and normal hardware config live in original Rust source,
not behind a host-only feature. The platform owns its shared firmware feature
bundle; the board has no forwarding recipe, direct stage runtime dependency or
host executable. Editor projects remain Cargo-derived, target-bounded and
canonical original-source graphs.

## Acceptance evidence

Evidence is recorded in the implementation environment, not shipped as generated
board configuration. Final compiler and regression commands are collected in
`/tmp/fstart-common-final-builds.sh`.

- The original 59 focused tests pass in `/tmp/fstart-common-tests.log`.
  The cfg-schema/environment-isolation review adds three tests (62 total),
  including subprocess-inherited environment checks:
  `/tmp/fstart-common-review-tests.log`.
- Follow-up compiler/editor and actual fresh/cached locked-host checks:
  `/tmp/fstart-common-review-ide.log`, `/tmp/fstart-common-review-live-ra.log`,
  `/tmp/fstart-common-review-host.log`. These supplement the release/boot evidence
  below; the follow-up changes the host contract and environment isolation, not
  hardware or runtime layout policy.
- X61 halt/UEFI and relocating AArch64 halt release assembly:
  `/tmp/fstart-common-acceptance-builds.log`.
- Exact comparison to the typed-facts baseline:
  `/tmp/fstart-common-image-proof.json`: all three descriptors and effective
  ranges match, as do the current 11,856-byte SMM image (embedded once), the
  86,016-byte microcode blob, and 2,621,440 erased non-BIOS bytes.
- Fresh AArch64 emulator boot through integrity, device tree, PCI and
  ready-for-payload: `/tmp/fstart-common-aarch64-boot.log` and
  `/tmp/fstart-common-aarch64-boot-proof.json`.
- Live Intel car → postcar → ram → SMM → AArch64 selection switching, original
  fact navigation, hygienic macro expansion, compiler/RA E0080 for overlapping
  IFD and invalid physical flash capacity, followed by restored compiler checks:
  `/tmp/fstart-common-editor-proof.log` and `/tmp/fstart-common-editor-proof/`.
- Actual fresh/cached locked host Cargo graphs for both platforms:
  `/tmp/fstart-common-host-proof.log` and `/tmp/fstart-common-host-proof/`.
- Additional AArch64 UEFI assembly and representative legacy releases:
  `/tmp/fstart-common-aarch64-uefi.log`, `/tmp/fstart-common-legacy-{i945,pineview,riscv64,armv7}.log`.

The synthetic selection tests intentionally shuffle non-family unit names,
use different per-unit targets/entries/features, select only needed producers,
reject missing artifacts and prove changed producer bytes/configuration change
consumer output identity. Real LLD tests retain the AArch64 copy-extent and
fixed-overflow regressions while consuming the typed QEMU plan, not a deleted
Cargo recipe.

No hardware boot, stack high-water measurement or full board-matrix claim.
X61 generated images still contain **erased non-BIOS IFD/GbE/ME ranges** and are
not factory/full-chip backups. Do not blindly flash the entire generated image.
