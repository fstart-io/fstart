# fstart Continuation Plan

Status as of 2026-02-06 (updated Phase 7 complete). This document captures
what has been built, what remains, and the recommended order of work for
future sessions.

## What Is Done

### Phase 1: Foundation (COMPLETE)

All 14 workspace crates created and cross-compiling for both targets.

- `Device` trait with associated `type Config`, `DeviceError` enum — in
  `fstart-services/src/device.rs`.
- `BusDevice` trait (marker for bus-attached devices with `type ParentBus`) —
  in `fstart-services/src/device.rs`.
- `Ns16550Config`, `Pl011Config` typed config structs — in their respective
  driver files under `fstart-drivers/src/uart/`.
- `Device` implemented for both `Ns16550` and `Pl011` with `new()`, `init()`,
  `NAME`, `COMPATIBLE`, `type Config`.
- `parent: Option<HString<32>>` field on `DeviceConfig` for bus hierarchies.
- Old `Driver` trait removed (never existed — clean start).

### Phase 2: Codegen Upgrade (COMPLETE)

- `Devices` struct generated with concrete typed fields per device.
- `StageContext` generated with service accessor methods (`console()`,
  `block_device()`, `timer()`) returning `&(impl Trait + '_)`.
- Typed `Config` construction: codegen maps RON `Resources` → driver-specific
  config structs at build time.
- `fstart_capabilities::console_ready()` wired into generated code.
- Codegen validation with `compile_error!()` for: unknown drivers, missing
  device references, service mismatches, missing required resources.

### Phase 3: Capability Pipeline (COMPLETE)

All capability functions implemented in `fstart-capabilities/src/lib.rs`:

| Function | Purpose | Status |
|----------|---------|--------|
| `console_ready()` | Log console banner after ConsoleInit | Real impl |
| `memory_init()` | DRAM init (no-op on QEMU, logs) | Stub with logging |
| `driver_init_complete()` | Log DriverInit phase completion | Real impl |
| `sig_verify()` | FFS manifest signature check | Stub with logging |
| `fdt_prepare()` | Generate FDT for OS handoff | Stub with logging |
| `payload_load()` | Load and jump to payload | Stub with logging |
| `stage_load()` | Load next stage from FFS | Stub with logging |

Codegen generates real function calls for all capabilities (no more `// TODO:`
stubs). The generated `fstart_main()` now:

1. Constructs all devices via `Device::new()` with typed configs.
2. Executes capabilities in RON-declared order, passing console references.
3. `DriverInit` tracks which devices were already inited (e.g., by
   `ConsoleInit`) and skips them to avoid double-init.
4. Builds `StageContext` with all devices.
5. Logs "all capabilities complete" via the context's console accessor.
6. Halts.

**Capability ordering validation**: codegen emits `compile_error!()` if any
capability that needs logging (all except `ConsoleInit`) appears before
`ConsoleInit` in the capability list.

**8 unit tests** in `fstart-codegen/src/stage_gen.rs` covering:
- Console init generates device init + banner
- MemoryInit after ConsoleInit generates correct call
- MemoryInit without ConsoleInit is a compile error
- DriverInit skips already-inited devices
- SigVerify, StageLoad generate correct calls
- Unknown driver produces compile error
- Completion message is always present

### Phase 4: Bus Support (COMPLETE)

Bus-attached device infrastructure with parent-before-child init ordering.

#### Bus Service Traits

Three new service traits in `fstart-services/src/`:
- `I2cBus` (`i2c.rs`) — `read(addr, reg, buf)`, `write(addr, reg, data)`
- `SpiBus` (`spi.rs`) — `transfer(cs, tx, rx)`, default `write()` / `read()`
- `GpioController` (`gpio.rs`) — `get(pin)`, `set(pin, value)`, `set_direction(pin, output)`

All re-exported from `fstart-services::lib.rs`.

#### DesignWare I2C Controller Driver

Full driver in `fstart-drivers/src/i2c/designware.rs`:
- Complete register block via `register_structs!` / `register_bitfields!`,
  matching the Synopsys DW_apb_i2c databook (cross-referenced with coreboot
  and U-Boot implementations)
- `DesignwareI2cConfig { base_addr, clock_freq, bus_speed }` with
  `I2cSpeed` enum (Standard 100kHz / Fast 400kHz)
- `impl Device for DesignwareI2c` with SCL timing calculation from clock freq
- `impl I2cBus for DesignwareI2c` with polled master-mode 7-bit addressing
- Feature-gated under `designware-i2c`

#### Topological Sort in Codegen

`stage_gen.rs` now:
- Sorts devices topologically (Kahn's algorithm) before code generation
- Validates every `parent` reference names an existing device → `compile_error!`
- Validates parent provides a bus service (I2cBus, SpiBus, GpioController) →
  `compile_error!`
- Detects cycles in parent chain → `compile_error!`
- Generates device construction and `DriverInit` calls in parent-before-child
  order

#### Codegen Enhancements

- `designware-i2c` added to driver registry with config field mapping
  (`bus_speed` Hz → `I2cSpeed` enum)
- `Resources.bus_speed: Option<u32>` added to `fstart-types` for bus
  controller speed configuration
- `StageContext` generates `i2c_bus()`, `spi_bus()`, `gpio()` accessors
  when corresponding services are present
- Import generation adds `use fstart_services::I2cBus` etc. when needed

#### Testing

**18 unit tests** in `fstart-codegen/src/stage_gen.rs` (8 original + 10 new):
- Topological sort with no parents (all roots)
- Topological sort reorders parent before child
- Unknown parent reference → compile error
- Parent without bus service → compile error
- Cycle detection → compile error
- I2C bus generates correct DesignwareI2cConfig
- I2C bus generates I2cBus import
- I2C bus generates `i2c_bus()` accessor
- DriverInit with bus hierarchy inits parent before child
- Parent reference to unknown device → compile error in full codegen

### Infrastructure Fixes

- **AArch64 objcopy**: `xtask build` now automatically runs `llvm-objcopy
  -O binary` for AArch64 boards (QEMU `-bios` expects flat binary, not ELF).
- **`cargo xtask run --board qemu-aarch64`** now works end-to-end.

### Phase 5: Flexible Mode (COMPLETE)

Enum dispatch codegen for runtime driver selection — no trait objects, no alloc.

#### Service Enum Generation

When `mode: Flexible`, codegen generates service enum wrappers for each
service trait that has drivers in the board. For example, a board with both
NS16550 and PL011 consoles gets:

```rust
enum ConsoleDevice {
    Ns16550(Ns16550),
    Pl011(Pl011),
}

impl Console for ConsoleDevice {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        match self {
            Self::Ns16550(d) => d.write_byte(byte),
            Self::Pl011(d) => d.write_byte(byte),
        }
    }
    // ...
}
```

All 6 service traits (Console, BlockDevice, Timer, I2cBus, SpiBus,
GpioController) have full enum dispatch metadata defined in `SERVICE_TRAITS`.

#### Two-Phase Construction

In flexible mode, device construction is split into two phases to support
the `Device::init()` trait method (which is on the concrete type, not the
enum):

1. **Construct**: `let _uart0_inner = Ns16550::new(&Ns16550Config { ... })`
2. **Init**: `_uart0_inner.init()` (ConsoleInit or DriverInit capability)
3. **Wrap**: `let uart0 = ConsoleDevice::Ns16550(_uart0_inner)`

This ensures `init()` is called on the concrete type before enum wrapping.

#### Codegen Changes

- `generate_imports()` adds `use fstart_services::ServiceError` in Flexible mode
- `generate_flexible_enums()` generates enum types + trait impls per service
- `generate_devices_struct()` uses enum types instead of concrete types
- `generate_stage_context()` returns `&EnumType` instead of `&(impl Trait + '_)`
- `generate_device_construction()` uses `_name_inner` temporary variables
- `generate_console_init()` wraps after init via `generate_flexible_wrapping()`
- `generate_driver_init()` wraps after init for each remaining device
- `DriverInfo.services` field now used (removed `#[allow(dead_code)]`)

#### Testing Board

New board `boards/qemu-riscv64-flex/board.ron` — identical to `qemu-riscv64`
but with `mode: Flexible`. Boots on QEMU with correct output.

#### Testing

**29 unit tests** in `fstart-codegen/src/stage_gen.rs` (18 original + 11 new):
- Flexible generates ConsoleDevice enum
- Flexible generates Console trait impl with delegation
- Multi-driver board enum has both Ns16550 and Pl011 variants
- Devices struct uses enum type in flexible mode
- StageContext returns &ConsoleDevice (not &impl Console)
- Construction uses `_inner` variable pattern
- ServiceError imported in flexible mode
- DriverInit wraps after init
- Completion message present in flexible mode
- I2C bus generates I2cBusDevice enum with trait impl
- Rigid mode unchanged — no enums generated

### Phase 6: Firmware Filesystem + Security (COMPLETE)

Full FFS format redesign with RO/RW A/B flash layout, crypto verification,
builder, reader, and `xtask assemble` command.

#### Flash Layout Model

The firmware image uses a layered flash layout:

```text
┌─────────────────────────────────────────────────────────┐
│ Bootblock (XIP from flash)                              │
│  ┌────────────────────────────────────────────────────┐  │
│  │ Anchor Block (embedded in bootblock binary)        │  │
│  │  • MAGIC: "FSTART01"                               │  │
│  │  • pointer → RO manifest                           │  │
│  │  • ro_region_base (for segment offset calculation)  │  │
│  │  • embedded verification keys                      │  │
│  └────────────────────────────────────────────────────┘  │
│  … bootblock code …                                     │
├─────────────────────────────────────────────────────────┤
│ RO Region (immutable firmware)                          │
│  • RO Manifest (signed)  ← anchor points here           │
│  • immutable files (stages, data, …)                    │
│  • optional pointers → RW-A, RW-B manifests             │
├─────────────────────────────────────────────────────────┤
│ RW-A Region (optional, updatable)                       │
│  • RW Manifest (signed by keys in anchor)               │
│  • stage code, payloads, data                           │
├─────────────────────────────────────────────────────────┤
│ RW-B Region (optional, A/B safe update)                 │
│  • RW Manifest (signed by keys in anchor)               │
│  • stage code, payloads, data                           │
├─────────────────────────────────────────────────────────┤
│ NVS Region (optional, plain storage)                    │
└─────────────────────────────────────────────────────────┘
```

Key design decisions:
- **Anchor is embedded in the bootblock binary** — not loaded via SPI driver.
  The bootblock is XIP from memory-mapped flash, so the anchor is just part
  of the code image. No driver needed to read it.
- **RW regions are optional**: RO-only, RO+RW, or RO+RW-A+RW-B.
- **Files have multiple segments** (`.text`, `.data`, `.rodata`, `.bss`)
  with per-segment compression, load address, and memory flags (execute,
  write, read) for future paging/MPU support.
- **File types**: StageCode, Payload, BoardConfig, Fdt, Data, Nvs, Raw.
- **Dual digests**: SHA-256 + SHA3-256 (algorithm agility).
- **Signature agility**: Ed25519 + ECDSA P-256, with key_id for rotation.

#### 6.1 fstart-types FFS Redesign

Complete rewrite of `ffs.rs`:
- `AnchorBlock`: MAGIC, version, RO manifest pointer, `ro_region_base`,
  embedded `VerificationKey`s
- `VerificationKey`: key_id, algorithm, key material (split for serde)
- `SignedManifest`: raw manifest bytes + `Signature`
- `Manifest`: region role, file entries, RW slot pointers, NVS pointer
- `RegionRole`: Ro, RwA, RwB, Rw
- `RwSlotPointer`: manifest offset/size, region base/size
- `FileEntry`: name, file_type, segments, digests
- `Segment`: name, kind, offset, stored_size, loaded_size, load_addr,
  compression, flags
- `SegmentKind`: Code, ReadOnlyData, ReadWriteData, Bss
- `SegmentFlags`: execute, write, read (with `CODE`, `RODATA`, `DATA` consts)
- `Signature`: key_id, kind, sig_lo/sig_hi (unified struct, not enum)
- `SignatureKind`: Ed25519, EcdsaP256
- `FfsHeader`: legacy compat for simple single-region images

#### 6.2 fstart-crypto

`crates/fstart-crypto/` — no_std crypto primitives:
- **`digest` module**: `hash_sha256()`, `hash_sha3_256()`, `hash_digest_set()`,
  `verify_digest_set()` — all behind `sha2-digest` / `sha3-digest` features
- **`verify` module**: `verify_signature()`, `verify_with_key_lookup()` —
  Ed25519 via `ed25519-dalek` (`ed25519` feature), ECDSA P-256 via `p256`
  (`ecdsa-p256` feature)
- Feature-gated: `ed25519`, `ecdsa-p256`, `sha2-digest`, `sha3-digest`, `all`
- Error types: `DigestError`, `VerifyError`

#### 6.3 fstart-ffs Reader (no_std)

`crates/fstart-ffs/src/reader.rs` — reads from `&[u8]` flash image:
- `FfsReader::new(image)` — borrows the memory-mapped image
- `read_anchor(offset)` — deserialize anchor at known offset
- `scan_for_anchor()` — scan for MAGIC at 8-byte-aligned offsets
- `read_ro_manifest(anchor)` — read + verify RO manifest signature
- `read_rw_manifest(slot, anchor)` — read + verify RW manifest
- `find_rw_slot(manifest, role)` — find RW slot pointer by role
- `find_file(manifest, name)` — look up file by name
- `read_segment_data(segment, region_base)` — read segment data
- `verify_file_digests_raw(file, region_base)` — verify in-place digests
- `ReaderError::CannotVerifyInPlace` — explicit error for multi-segment/compressed
  files that cannot be verified without decompression

#### 6.4 fstart-ffs Builder (std)

`crates/fstart-ffs/src/builder.rs` — assembles FFS images:
- `build_image(config, sign_fn)` — generic over signing function
- `FfsImageConfig`: keys, RO region, RW regions, NVS size
- `InputFile` / `InputSegment`: file + segment data for assembly
- Lay out files, compute digests, serialize manifests, sign, produce image
- `FfsImage`: final image bytes + anchor offset + anchor bytes + RO region base

#### 6.5 xtask assemble

`xtask/src/assemble.rs` — `cargo xtask assemble --board <name>`:
- Reads board RON config
- Builds all stages (monolithic or multi-stage)
- Generates or loads Ed25519 dev key pair from `boards/<name>/keys/`
- Builds RO-only FFS image with signed manifest
- Outputs to `target/ffs/<board-name>.ffs`

#### Crypto Feature Forwarding

`fstart-ffs` now forwards crypto features for no_std firmware builds:
- `ed25519` → `fstart-crypto/ed25519`
- `sha2-digest` → `fstart-crypto/sha2-digest`
- `sha3-digest` → `fstart-crypto/sha3-digest`
- `std` feature enables `fstart-crypto/all` (all algorithms)

#### Testing

**9 integration tests** in `crates/fstart-ffs/tests/round_trip.rs`:
- RO-only round trip (build → scan → read → verify → data)
- Multi-segment file (`.text` + `.rodata` + `.data`)
- Multiple files in RO region (lookup by name)
- RO with single RW slot (RW manifest verification + data read)
- RO with RW-A + RW-B A/B slots (both independently verified)
- NVS region (pointer present, erased flash 0xFF)
- Tampered signature detection (bit-flip → verification failure)
- Wrong key detection (different key → verification failure)
- File not found error

### Phase 7: Multi-Stage Layout (COMPLETE)

Full multi-stage build support: bootblock (XIP/RAM) + main stage (RAM).

#### Multi-Stage Build Pipeline

`StageLayout::MultiStage` is now fully functional end-to-end:

1. **Board RON** declares multiple stages with per-stage capabilities,
   load addresses, stack sizes, and `runs_from` (Rom/Ram).
2. **`cargo xtask build --board <name>`** iterates over all stages, building
   each with `FSTART_STAGE_NAME` set. Each produces a separate binary
   (e.g., `fstart-bootblock`, `fstart-main`).
3. **`cargo xtask assemble --board <name>`** builds all stages, then packages
   them into a single signed FFS image with one `FileEntry` per stage.
4. **`cargo xtask run --board <name>`** boots the first (primary) stage on QEMU.

#### xtask Build Orchestration

`xtask/src/build_board.rs` rewritten:
- `BuildResult` struct with `Vec<StageBinary>` — name, path, load_addr.
- `build()` returns `BuildResult` (breaking change from `PathBuf`).
- `build_one_stage()` builds a single `fstart-stage` binary.
- Multi-stage: copies each output to `fstart-<stage-name>` to avoid
  overwrites (cargo always outputs to `fstart-stage`).
- `primary_binary()` helper returns the first stage for QEMU boot.

#### xtask Assemble Multi-Stage

`xtask/src/assemble.rs` updated:
- Builds all stages via `build()` before assembly.
- For monolithic boards: one `InputFile` in the RO region.
- For multi-stage boards: one `InputFile` per stage, each with load_addr
  from the stage config.
- Logs the number of stages packaged.

#### Codegen: Stage Ending Behavior

`stage_gen.rs` now detects whether a stage ends with a jump capability
(`StageLoad` or `PayloadLoad`):
- **Jump stages** (bootblock): no "all capabilities complete" message.
  After the jump capability, emits a halt (reached only if the stub
  returns, which real implementations won't).
- **Terminal stages** (main): normal completion message + halt.

#### Testing Board

New board `boards/qemu-riscv64-multi/board.ron`:
- Two stages: `bootblock` (ConsoleInit + SigVerify + StageLoad → main)
  and `main` (ConsoleInit + MemoryInit + DriverInit).
- Both stages at different load addresses (0x80000000 and 0x80100000).
- Rigid mode, RISC-V 64, NS16550 UART.

#### Testing

**6 new unit tests** in `fstart-codegen/src/stage_gen.rs` (35 total):
- Multi-stage bootblock generates ConsoleInit + SigVerify + StageLoad
- Multi-stage bootblock does NOT log completion (ends with StageLoad)
- Multi-stage main stage generates ConsoleInit + MemoryInit + DriverInit + completion
- Multi-stage without FSTART_STAGE_NAME → compile_error
- Multi-stage with unknown stage name → compile_error
- Stage ending with PayloadLoad does NOT log completion

### Verified Working

| Board | Mode | Output |
|-------|------|--------|
| qemu-riscv64 debug | `cargo xtask run --board qemu-riscv64` | `[fstart] uart0: ns16550 console ready` + `[fstart] all capabilities complete` |
| qemu-riscv64 release | `cargo xtask build --board qemu-riscv64 --release` | Builds clean |
| qemu-aarch64 debug | `cargo xtask run --board qemu-aarch64` | `[fstart] uart0: pl011 console ready` + `[fstart] all capabilities complete` |
| qemu-aarch64 release | `cargo xtask build --board qemu-aarch64 --release` | Builds clean |
| qemu-riscv64-flex debug | `cargo xtask run --board qemu-riscv64-flex` | `[fstart] uart0: ns16550 console ready` + `[fstart] all capabilities complete` |
| qemu-riscv64-multi debug | `cargo xtask build --board qemu-riscv64-multi` | Builds bootblock + main |
| qemu-riscv64-multi bootblock | QEMU boot | `[fstart] uart0: ns16550 console ready` + SigVerify + StageLoad → main (stub) |
| qemu-riscv64-multi main | QEMU boot | `[fstart] uart0: ns16550 console ready` + MemoryInit + DriverInit + completion |
| qemu-riscv64-multi release | `cargo xtask build --board qemu-riscv64-multi --release` | 5.5 KB per stage |
| qemu-riscv64-multi FFS | `cargo xtask assemble --board qemu-riscv64-multi` | 5.7 MB FFS with 2 stages |
| qemu-riscv64 FFS | `cargo xtask assemble --board qemu-riscv64` | 2.8 MB FFS with 1 stage |
| clippy | `cargo clippy --workspace --exclude fstart-stage -- -D warnings` | Clean |
| fmt | `cargo fmt --all -- --check` | Clean |
| tests | `cargo test --workspace --exclude fstart-stage --exclude fstart-runtime --exclude fstart-platform-*` | 44 pass (35 codegen + 9 FFS) |

---

## What Remains

### Explore crabtime (Optional)

Investigate `crabtime` (Zig comptime-like macros for Rust) as an
alternative or complement to `build.rs` codegen for enum dispatch
generation. Low priority — current codegen approach works well.

---

### Phase 8: Payload + OS Handoff

**Goal**: Boot Linux on QEMU.

#### 8.1 FDT Generation

- Generate FDT from board RON (memory map, devices, chosen node).
- `DTS Override` escape hatch: merge board-provided DTS fragments.
- Place FDT at known address for payload.

#### 8.2 Linux Boot Protocol

- RISC-V: OpenSBI-style boot (a0=hartid, a1=fdt_addr, jump to kernel).
- AArch64: kernel image protocol (x0=fdt_addr, jump to kernel).

#### 8.3 Test

- Package a minimal Linux kernel (or test payload) in FFS.
- `cargo xtask run --board qemu-riscv64` boots to kernel banner.

---

### Phase 9: Wire Capabilities to FFS

**Goal**: Connect stub capabilities to real FFS operations.

#### 9.1 Anchor Embedding in Bootblock

- Codegen emits a `#[link_section = ".anchor"]` static containing the
  serialized `AnchorBlock` with keys read from the board's pubkey file
  at build time. For now this is a placeholder — `xtask assemble` patches
  manifest offsets after image layout.

#### 9.2 SigVerify → FFS Reader

- `sig_verify()` receives the flash image base address (from platform code
  or a fixed constant in the board config).
- Creates an `FfsReader`, reads the anchor, verifies the RO manifest
  signature, and checks file digests.

#### 9.3 StageLoad → FFS + Jump

- `stage_load()` looks up the next stage in the FFS manifest, reads its
  segments, copies to the load address, and jumps via a platform-specific
  `jump_to(addr)` function.

#### 9.4 PayloadLoad → FFS + OS Boot

- `payload_load()` reads the payload from FFS, loads it, sets up FDT,
  and transfers control using the platform boot protocol.

---

### Phase 10: Polish + CI

- Logging infrastructure (`fstart-log`): structured log levels, compile-time
  filtering.
- Allocator (`fstart-alloc`): bump allocator for stages that need heap.
- CI pipeline: GitHub Actions with `cargo check`, `clippy`, `fmt`, `test`,
  cross-build all boards.
- Measured boot hooks: TPM event log placeholder.
- More drivers: SiFive UART, VirtIO block, etc.

---

## File Summary (Phase 7 Changes)

| File | Change |
|------|--------|
| `crates/fstart-types/src/ffs.rs` | Added `ro_region_base: u32` to `AnchorBlock` |
| `crates/fstart-ffs/src/reader.rs` | Added `CannotVerifyInPlace` error, fixed doc comment for `read_segment_data` |
| `crates/fstart-ffs/src/builder.rs` | Includes `ro_region_base` in anchor block |
| `crates/fstart-ffs/Cargo.toml` | Added `ed25519`, `sha2-digest`, `sha3-digest` feature forwarding |
| `crates/fstart-ffs/tests/round_trip.rs` | Uses `anchor.ro_region_base` in RO round-trip test |
| `crates/fstart-codegen/src/stage_gen.rs` | Smart stage ending (jump vs terminal), 6 new multi-stage tests |
| `xtask/src/build_board.rs` | **Rewritten**: `BuildResult`/`StageBinary` types, per-stage builds, binary renaming |
| `xtask/src/main.rs` | Updated for `BuildResult` API |
| `xtask/src/assemble.rs` | **Rewritten**: builds + packages all stages, multi-stage FFS assembly |
| `boards/qemu-riscv64-multi/board.ron` | **New**: multi-stage board (bootblock + main) |
| `docs/continuation-plan.md` | Updated with Phase 7 completion |

## Git State

Seven commits on `master`:
1. `1b2b71f` — Initial commit: fstart firmware framework with 14 workspace crates
2. `9383113` — Introduce typed Device trait driver model with codegen-produced StageContext
3. `de59c64` — Capability pipeline, BusDevice trait, codegen ordering validation, AArch64 objcopy
4. `187f4af` — Bus support: I2C/SPI/GPIO service traits, DesignWare I2C driver, topological sort
5. `6e6fd82` — Flexible mode: enum dispatch codegen for runtime driver selection
6. (Pending) — Phase 6: FFS + Security — flash layout, crypto, reader, builder, xtask assemble
7. (Pending) — Phase 7: Multi-stage layout — per-stage builds, FFS packaging, bootblock + main

## Quick Reference: Build Commands

```bash
# Host checks (fast)
cargo check --workspace --exclude fstart-stage
cargo clippy --workspace --exclude fstart-stage -- -D warnings
cargo fmt --all -- --check
cargo test --workspace --exclude fstart-stage --exclude fstart-runtime \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64

# Cross-build boards
cargo xtask build --board qemu-riscv64
cargo xtask build --board qemu-aarch64
cargo xtask build --board qemu-riscv64-flex
cargo xtask build --board qemu-riscv64-multi
cargo xtask build --board qemu-riscv64 --release
cargo xtask build --board qemu-riscv64-multi --release

# Assemble FFS images
cargo xtask assemble --board qemu-riscv64
cargo xtask assemble --board qemu-riscv64-multi

# Run on QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64
cargo xtask run --board qemu-riscv64-flex
cargo xtask run --board qemu-riscv64-multi  # boots bootblock stage
```
