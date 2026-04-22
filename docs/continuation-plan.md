# fstart Continuation Plan

Status as of 2026-02-18 (updated Phase 11 complete — Linux boots on both
QEMU targets). This document captures what has been built, what remains,
and the recommended order of work for future sessions.

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

### Phase 2: Codegen Upgrade (COMPLETE — superseded by Phase 13)

> **Note:** The `Devices` struct and `StageContext` described here were
> replaced by `_BoardDevices` + `impl Board` + `run_stage()` in Phase 13
> (stage-runtime / codegen split).  The typed `Config` construction and
> codegen validation remain unchanged.

- ~~`Devices` struct generated with concrete typed fields per device.~~ → replaced by `_BoardDevices`
- ~~`StageContext` generated with service accessor methods (`console()`,
  `block_device()`, `timer()`) returning `&(impl Trait + '_)`.~~ → removed
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

### Phase 5: Flexible Mode (REMOVED — superseded by Phase 13)

> **Note:** Flexible mode was implemented and then **deleted** when the
> stage-runtime / codegen split landed in Phase 13.  The generic `Board`
> trait makes enum dispatch redundant — if runtime driver selection is
> ever needed, it lives inside `_BoardDevices` fields, not a separate
> codegen mode.  `flexible.rs`, `qemu-riscv64-flex` board, and the
> `BuildMode::Flexible` variant are all gone.  Only `BuildMode::Rigid`
> remains.

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

### Phase 9: Wire Capabilities to FFS (COMPLETE)

Real FFS operations wired into capability functions. The bootblock now
performs genuine FFS anchor scanning, manifest signature verification,
file digest checking, and stage loading.

#### Platform `jump_to(addr)`

Both platform crates (`fstart-platform-riscv64`, `fstart-platform-aarch64`)
now provide `jump_to(addr: u64) -> !`:
- RISC-V: `jr` instruction
- AArch64: `br` instruction

Used by `StageLoad` and `PayloadLoad` to transfer control to loaded code.

#### `flash_base` / `flash_size` in Board Config

New optional fields in `MemoryMap`:
- `flash_base: Option<u64>` — where the firmware image is mapped in memory
- `flash_size: Option<u64>` — total firmware image size for FFS reader bounds

All 4 board RON files updated with appropriate values. For QEMU riscv64,
this is the RAM address where `-bios` loads the image (0x80000000). For
QEMU aarch64, this is the flash base (0x00000000).

#### Codegen Changes

- `generate_flash_constants()` emits `FLASH_BASE` and `FLASH_SIZE` constants
  when any FFS capability is present and `flash_base`/`flash_size` are configured.
- `generate_sig_verify()` now passes `FLASH_BASE, FLASH_SIZE` (or `0, 0` fallback).
- `generate_stage_load()` passes `FLASH_BASE, FLASH_SIZE, platform::jump_to`
  when FFS is configured; falls back to `stage_load_stub()` otherwise.
- `generate_payload_load()` same pattern with `payload_load_stub()` fallback.
- `needs_ffs()` helper detects FFS capabilities in the capability list.
- `generate_imports()` accepts capabilities param (no longer imports FfsReader
  directly — FFS operations are handled inside `fstart_capabilities`).
- Linker script updated with `.fstart.anchor` section (8-byte aligned, after
  `.text`) in both XIP and RAM layouts.

#### Feature Flag Plumbing

**`fstart-capabilities`** new features:
- `ffs` — enables `fstart-ffs` and `fstart-types` dependencies
- `ed25519` — forwards to `fstart-ffs/ed25519`
- `sha2-digest` / `sha3-digest` — forwards to `fstart-ffs` crypto features

**`fstart-stage`** new features:
- `ffs` — forwards to `fstart-capabilities/ffs`
- `ed25519`, `sha2-digest`, `sha3-digest` — forward crypto features

**`xtask/src/build_board.rs`** auto-detects:
- Scans all stages for FFS capabilities (SigVerify, StageLoad, PayloadLoad)
- Automatically enables `ffs` feature + crypto features matching the board's
  `security.signing_algorithm` and `security.required_digests`

#### Real Capability Implementations (behind `ffs` feature)

**`sig_verify(console, flash_base, flash_size)`:**
1. Creates `FfsReader` over the memory-mapped flash image
2. Scans for `FFS_MAGIC` at 8-byte-aligned offsets
3. Reads and validates the `AnchorBlock`
4. Reads and cryptographically verifies the RO manifest signature
5. Verifies file digests for single-segment uncompressed files
6. Logs results: files verified, files skipped (multi-segment)
7. Gracefully handles "no FFS image" (no anchor found → skip)

**`stage_load(console, next_stage, flash_base, flash_size, jump_to)`:**
1. Scans for anchor and reads verified RO manifest
2. Looks up the named stage file in the manifest
3. Copies all segments to their load addresses (BSS zeroed)
4. Jumps to the first Code segment's load address via `jump_to()`
5. Gracefully handles missing stage file

**`payload_load(console, flash_base, flash_size, jump_to)`:**
1. Same flow as `stage_load` but looks for `FileType::Payload`
2. Loads segments and jumps to entry point

**Shared helpers** (behind `ffs` feature):
- `find_anchor_and_manifest()` — scan + read + verify, with error logging
- `load_file_segments()` — copy all segments, handle BSS, detect compression
- `reader_error_str()` — map `ReaderError` to `&'static str` for logging
- `write_hex()` — format `u64` as `0x...` for no_std console output

**Stubs** (when `ffs` feature absent or `flash_base` not configured):
- `sig_verify` with `ffs` disabled logs "ffs feature not enabled"
- `stage_load_stub` / `payload_load_stub` log "not yet wired to FFS"

#### Testing

**3 new unit tests** in `fstart-codegen/src/stage_gen.rs` (38 total):
- `test_sig_verify_with_flash_base_generates_constants` — verifies FLASH_BASE/SIZE
  constants emitted and sig_verify called with them
- `test_stage_load_with_flash_base_generates_real_call` — verifies stage_load
  called with FLASH_BASE/SIZE and jump_to
- `test_multi_stage_bootblock_with_flash_base` — full bootblock codegen with
  real FFS calls and flash constants

**Updated 3 existing tests** for new function signatures:
- `test_sig_verify_generates_call` — uses `0, 0` fallback args
- `test_stage_load_generates_call` — uses `stage_load_stub`
- `test_multi_stage_bootblock_generates_stage_load` — uses stub variants

### Phase 10: ufmt Logging Infrastructure (COMPLETE)

Replaced manual `console.write_str()` chains with ergonomic `ufmt`-backed
log macros across the entire codebase.

#### fstart-log Crate

Rewrote the empty skeleton (`crates/fstart-log/src/lib.rs`):
- `Level` enum (Error, Warn, Info, Debug, Trace) with runtime filtering
  via `max_level()` / `set_max_level()`
- Global console backend using `SyncCell<UnsafeCell<T>>` wrapper (Rust
  2024 forward-compatible — avoids deprecated `static mut`)
- `init()` with double-init guard (silently ignores second call)
- `ConsoleWriter` — zero-sized `ufmt::uWrite` adapter routing to global
  console; silently discards if no console registered
- `Hex(u64)` wrapper implementing `ufmt::uDisplay` for hex formatting
- `error!`, `warn!`, `info!`, `debug!`, `trace!` macros with `[LEVEL]`
  prefix and `ufmt::uwriteln!` delegation
- Re-exports `ufmt` via `#[doc(hidden)] pub use ufmt` for macro consumers

#### fstart-capabilities Migration

Full rewrite of `crates/fstart-capabilities/src/lib.rs`:
- Removed `console: &dyn Console` parameter from all 13 public + 2
  internal function signatures
- Replaced ~100 manual write chains with `info!`/`error!`/`debug!` macros
- Deleted `write_usize()` and `write_hex()` helpers
- Dropped `fstart-services` dependency (no longer needed)

#### Codegen Updates

`crates/fstart-codegen/src/stage_gen.rs`:
- `generate_console_init` emits `unsafe { fstart_log::init(&device) }`
  after device init
- All `generate_*` functions: removed `console_device` parameter, removed
  `&console` from capability calls
- `generate_payload_load_linux`: replaced `{con}.write_line(...)` with
  `fstart_log::info!(...)`
- Completion message uses `fstart_log::info!("all capabilities complete")`
- All 38 codegen tests updated

#### Workspace Dependency Changes

- Root `Cargo.toml`: replaced `log = "0.4"` with `ufmt = { version = "0.2",
  default-features = false }`
- `fstart-stage/Cargo.toml`: added `fstart-log` + `ufmt`
- `fstart-capabilities/Cargo.toml`: replaced `fstart-services` with
  `fstart-log` + `ufmt`
- `fstart-log/Cargo.toml`: replaced `log` with `ufmt` + `fstart-services`

#### Design Notes

- **ufmt `$crate` quirk**: Any crate invoking `fstart_log` macros needs
  `ufmt` as a direct dependency because `ufmt::uwrite!` uses `$crate`
  internally. Both `fstart-stage` and `fstart-capabilities` have this.
- **Behavioral improvement**: Capabilities now always execute even without
  a console (log macros silently discard). Previously codegen silently
  omitted capability calls if no ConsoleInit preceded them.
- **No existing crate** combines ufmt formatting with leveled logging macros;
  `defmt` requires RTT probe tooling, `log` crate uses heavy `core::fmt`.

### Verified Working

| Board | Mode | Output |
|-------|------|--------|
| qemu-riscv64 debug | `cargo xtask run --board qemu-riscv64` | `[INFO ] uart0: ns16550 console ready` + `[INFO ] all capabilities complete` |
| qemu-riscv64 release | `cargo xtask build --board qemu-riscv64 --release` | Builds clean |
| qemu-aarch64 debug | `cargo xtask run --board qemu-aarch64` | `[INFO ] uart0: pl011 console ready` + `[INFO ] all capabilities complete` |
| qemu-aarch64 release | `cargo xtask build --board qemu-aarch64 --release` | Builds clean |
| qemu-riscv64-multi debug | `cargo xtask build --board qemu-riscv64-multi` | Builds bootblock + main |
| qemu-riscv64-multi bootblock | QEMU boot | Console ready + SigVerify + StageLoad stub |
| qemu-riscv64-multi main | QEMU boot | Console ready + MemoryInit + DriverInit + completion |
| qemu-riscv64-multi release | `cargo xtask build --board qemu-riscv64-multi --release` | Builds clean |
| qemu-riscv64-multi FFS | `cargo xtask assemble --board qemu-riscv64-multi` | FFS with 2 stages |
| qemu-riscv64 FFS | `cargo xtask assemble --board qemu-riscv64` | FFS with 1 stage |
| clippy | `cargo clippy --workspace --exclude fstart-stage -- -D warnings` | Clean |
| fmt | `cargo fmt --all -- --check` | Clean |
| tests | `cargo test --workspace --exclude fstart-stage --exclude fstart-runtime --exclude fstart-alloc --exclude fstart-platform-*` | 237 pass (68 codegen + 25 runtime + 14 FFS + ...) |

### Phase 11: Linux Boot Verified (COMPLETE)

Both QEMU targets boot Linux end-to-end through the full firmware chain.

#### RISC-V 64 (qemu-riscv64)

Boot chain: **fstart → RustSBI (M-mode) → Linux 6.18.0 (S-mode)**

```
cargo xtask run --board qemu-riscv64 --release \
    --kernel /tmp/linux-riscv64-embedded/arch/riscv/boot/Image \
    --firmware ~/src/rustsbi/target/riscv64gc-unknown-none-elf/release/rustsbi-prototyper-dynamic.bin
```

Firmware sequence:
1. fstart initialises NS16550 console
2. SigVerify: Ed25519 signature + SHA-256/SHA3-256 digest verification
3. FdtPrepare: copies QEMU DTB, patches `/chosen/bootargs` → `0x87F00000`
4. PayloadLoad: loads kernel (7.7 MiB Image → `0x82000000`),
   loads RustSBI fw_dynamic (236 KiB → `0x80100000`),
   prepares `FwDynamicInfo`, jumps to RustSBI
5. RustSBI initialises M-mode, `mret`s to Linux at `0x82000000` in S-mode
6. Linux boots, detects SBI v2.0, reaches `Run /init`

FFS image: 8.3 MiB (114 KiB firmware + 236 KiB RustSBI + 7.7 MiB kernel).

#### AArch64 (qemu-aarch64)

Boot chain: **fstart (EL3) → ATF BL31 (EL3) → Linux 6.18.0 (EL2)**

```
cargo xtask run --board qemu-aarch64 --release \
    --kernel /tmp/linux-arm64-embedded/arch/arm64/boot/Image \
    --firmware /tmp/arm-trusted-firmware/build/qemu/release/bl31.bin
```

Firmware sequence:
1. fstart initialises PL011 console
2. SigVerify: Ed25519 signature + SHA-256/SHA3-256 digest verification
3. FdtPrepare: copies QEMU DTB from `0x40000000`, patches bootargs → `0x40100000`
4. PayloadLoad: loads kernel (9.0 MiB Image → `0x41000000`),
   loads ATF BL31 (49 KiB → `0x0E090000`),
   prepares `BlParams` with BL33 entry point, jumps to BL31
5. ATF BL31 v2.14.0 initialises GICv3, `eret`s to Linux at `0x41000000` in EL2
6. Linux boots, detects GICv3, reaches `Run /init` (u-root)

FFS image: 9.5 MiB (103 KiB firmware + 49 KiB BL31 + 9.0 MiB kernel).

#### Bug Fix

- **QEMU AArch64 GIC version**: Added `gic-version=3` to QEMU `-machine`
  flags in `xtask/src/qemu.rs`. ATF BL31 is built with
  `QEMU_USE_GIC_DRIVER=QEMU_GICV3`; without explicit `gic-version=3`,
  QEMU may default to GICv2, causing BL31 to hang during GIC init.

### Phase 12: Platform Scalability Refactor (COMPLETE)

Replaced stringly-typed platform identity with a `Platform` enum, introduced
a codegen platform crate alias, and decoupled ARMv7 from Allwinner sunxi.
These changes make adding new platform targets (x86_64, LoongArch, etc.)
a compile-checked, mechanical process.

#### Platform Enum

`fstart-types/src/board.rs` — new `Platform` enum:
- Variants: `Riscv64`, `Aarch64`, `Armv7`
- Methods: `target_triple()`, `linker_arch()`, `as_str()`
- `Display` impl, serde derives, `Copy + PartialEq + Eq`
- `BoardConfig.platform` changed from `HString<32>` to `Platform`
- Exhaustive `match` everywhere — adding a new variant causes compile errors
  at every site that needs updating

#### Codegen Platform Alias

`generate_platform_externs()` in `stage_gen/mod.rs` now emits:
```rust
extern crate fstart_platform_riscv64 as fstart_platform;
```
Common calls (`halt()`, `jump_to()`) use `fstart_platform::` instead of
per-platform crate names. Platform-specific boot protocols (SBI, ATF, direct
Linux) still use the concrete platform crate where needed.

#### ARMv7 / Sunxi Decoupling

- `fstart-stage/Cargo.toml`: `armv7` feature no longer implies
  `dep:fstart-soc-sunxi`. A separate `sunxi` feature activates on either
  `aarch64` or `armv7` via `dep:fstart-soc-sunxi?` syntax.
- `fstart-soc-sunxi/Cargo.toml`: removed spurious `fstart-arch` dependency.
- This allows non-Allwinner ARMv7 boards (e.g., QEMU virt) to build without
  pulling in sunxi SoC code.

#### Scope of Changes

Eliminated string matching in **17 locations** across 6 files:
- `fstart-codegen/src/stage_gen/mod.rs` — function signatures, platform
  extern generation, import generation
- `fstart-codegen/src/stage_gen/capabilities.rs` — all 9 platform match
  sites for boot protocols, jump functions, memory init
- `fstart-codegen/src/stage_gen/tokens.rs` — `halt_expr()` now uses alias
- `fstart-codegen/src/linker.rs` — `linker_arch()` method replaces match
- `xtask/src/build_board.rs` — target triple, features, flat binary, sunxi
- `xtask/src/qemu.rs` — QEMU machine/CPU selection

All 10 board RON files updated: `platform: "riscv64"` → `platform: Riscv64`.

#### Verification

| Check | Result |
|-------|--------|
| `cargo test` | 71 pass (47 codegen + 14 FFS + 10 FIT) |
| `cargo clippy` | Clean (`-D warnings`) |
| qemu-riscv64 | Builds |
| qemu-aarch64 | Builds |
| qemu-armv7 | Builds |
| qemu-riscv64-multi | Builds |
| qemu-riscv64-flex | Builds |
| qemu-aarch64-multi | Builds |
| qemu-aarch64-flex | Builds |
| orangepi-pc2 | Builds |
| bananapi-m1 | Pre-existing linker overflow (bootblock too large for SRAM) |
| orangepi-r1 | Pre-existing linker overflow (bootblock too large for SRAM) |

---

## What Remains

### Platform Scalability — Deferred Items

These were identified during Phase 12 but deferred pending actual need:

1. **Uniform `boot_linux()` API** — Each platform crate exports different
   boot functions (`boot_linux_sbi`, `boot_linux_atf`, `boot_linux`).
   Unifying to a single `boot_linux(kernel, dtb, fw)` per-platform would
   reduce codegen duplication.  Low priority: `board_gen`’s
   `platform_boot_protocol_stmts` already abstracts the per-platform
   differences at codegen time.

2. **Abstract SoC boot header** — `LoadNextStage` and block-device
   boot-source detection are eGON-only.  A `SocBootHeader` trait would
   allow other SoCs (Rockchip, MediaTek) to plug in their own boot
   media detection and header format.  Deferred until a non-sunxi board
   actually needs it.

3. ~~**Move BROM mapping out of codegen**~~ — **DONE.** Moved to
   `DriverInstance::boot_media_values()` in `fstart-device-registry`.

4. ~~**Fix ARMv7 sunxi bootblock size**~~ — **DONE.** Added
   `[profile.dev.package.fstart-stage] opt-level = "s"` so debug
   bootblocks fit in SRAM.

5. ~~**AArch64 debug-mode hang**~~ — **DONE.** Pl011 driver now stores
   `base: usize` instead of `&'static Pl011Regs`, reconstructing the
   pointer via `#[inline(always)] fn regs()` on every access.

### Phase 13: Stage Runtime / Codegen Split (COMPLETE)

Replaced the old codegen architecture (generated `Devices` struct,
`StageContext`, inline `fstart_main()` body, `flexible.rs` enum dispatch)
with a clean split between handwritten runtime and codegen-emitted board
adapter.

#### New crate: fstart-stage-runtime

`crates/fstart-stage-runtime/` — `#![no_std]` handwritten executor:

- **`Board` trait** (20 methods): `init_device`, `init_all_devices`,
  `install_logger`, 12 capability trampolines (`memory_init`, `sig_verify`,
  `fdt_prepare`, `payload_load`, `stage_load`, `acpi_prepare`,
  `smbios_prepare`, `chipset_init`, `pci_init`, `acpi_load`,
  `memory_detect`, `return_to_fel`), boot media (`boot_media_select`,
  `boot_media_static`, `load_next_stage`), platform primitives (`halt`,
  `jump_to`, `jump_to_with_handoff`).
- **`StagePlan`** — `.rodata` literal with capability sequence as `CapOp`
  variants, `persistent_inited` / `boot_media_gated` / `all_devices` tables.
- **`CapOp` enum** — one variant per capability, device names resolved to
  `DeviceId` (`u8`).  No string comparisons at runtime.
- **`DeviceMask`** — 256-bit bitset over `DeviceId` for init tracking.
- **`BootMediaState`** — enum tracking the current boot medium (None / Mmio /
  Block) so trampolines can reconstruct the concrete `impl BootMedia`.
- **`run_stage<B: Board>(board, plan, handoff) -> !`** — one `match` per
  capability, dispatches through `Board` trait methods.  Monomorphised in
  Rigid mode — zero vtables.
- **25 host-side unit tests** via `MockBoard` + thread-local event log.

#### Codegen changes: plan_gen.rs + board_gen.rs

**`plan_gen.rs`** emits `static STAGE_PLAN: StagePlan` per stage:
- Resolves device names → `DeviceId` via `DeviceIdMap`.
- Emits `BootMediaCandidate` tables with `media_ids` for auto-select.
- `persistent_inited` from prior stages’ `ClockInit` / `DramInit`.
- `boot_media_gated` from multi-device `LoadNextStage` / `BootMediaAuto`.
- `all_devices` for `DriverInit` iteration.

**`board_gen.rs`** (4068 lines) emits the complete board adapter:
- `struct _BoardDevices` — `Option<Driver>` fields + bookkeeping
  (`_inited`, `_boot_media`, `_dtb_dst_addr`, `_bootargs`, `_dram_base`,
  `_dram_size_static`, `_handoff`, `_acpi_rsdp_addr`, `_egon_sram_base`).
- `impl _BoardDevices { const fn new() -> Self }` — all fields `None` / zero.
- `impl Board for _BoardDevices` — all 20 methods with real bodies:
  - `init_device`: per-device `match id` with ancestor-chain walking
    (root-first, bus-device `new_on_bus`).
  - `init_all_devices`: iterates enabled devices, respects skip/gated masks.
  - `install_logger`: per-Console-device match with `fstart_log::init`.
  - Capability trampolines: read board state from `&self` fields and
    delegate to `fstart_capabilities::*`.
  - Dead-code stubs (`todo!()`) for methods the stage doesn’t use.

#### What was deleted

- `flexible.rs` (468 lines) — Flexible mode enum dispatch.
- `generate_devices_struct` / `generate_stage_context` — old struct emission.
- `generate_fstart_main` — old inline `fstart_main()` body.
- `ensure_device_ready` / `walk_to_real_parent` / `make_prelude` — device
  construction chain building.
- `generate_driver_init` dispatch matrix.
- `generate_boot_media_auto_device` / `generate_load_next_stage` enum
  synthesis.
- `BuildMode::Flexible` variant (only `Rigid` remains).
- `qemu-riscv64-flex` board.

Result: `stage_gen/mod.rs` shrunk from 1371 → 530 lines;
`stage_gen/capabilities/mod.rs` shrunk from 984 → 120 lines.

#### Key design invariants

1. `STAGE_PLAN` is module-local (no `#[no_mangle]`) — allows future
   multi-platform binaries with multiple plans.
2. Only `fstart_main` calls `_BoardDevices::new()` — future
   `new_for(platform)` won’t touch the trait or executor.
3. No constants in `impl Board` method bodies — all on `self` fields.
4. Per-device init helpers via `chain_from_root` / `walk_to_real_parent`.
5. Capability trampolines take minimal context from executor.
6. `StagePlan` carries stage-composition data only, not platform data.

#### Verification

- 68 codegen tests + 25 runtime executor tests — all pass.
- All 16 boards build (13 debug, 3 release-only due to SRAM constraints).
- QEMU smoke test: firmware boots through full capability sequence.

#### Known limitation (resolved): AArch64 debug-mode hang

Previously, AArch64 debug builds hung during `Pl011::init()` because
LLVM routed the 16-byte struct through a stack scratch copy, causing
`init()` to program stale MMIO registers.  **Fixed** by storing
`base: usize` instead of `&'static Pl011Regs` in the Pl011 driver
and reconstructing the pointer via `#[inline(always)] fn regs()` on
every access.  AArch64 debug builds now work correctly.

---

### Explore crabtime (Optional)

Investigate `crabtime` (Zig comptime-like macros for Rust) as an
alternative or complement to `build.rs` codegen for enum dispatch
generation. Low priority — current codegen approach works well.

---

## File Summary (Phase 12 Changes — Platform Scalability)

| File | Change |
|------|--------|
| `crates/fstart-types/src/board.rs` | Added `Platform` enum, changed `BoardConfig.platform` type |
| `crates/fstart-types/src/lib.rs` | Added `Platform` to re-exports |
| `crates/fstart-codegen/src/ron_loader.rs` | `RonBoardConfig.platform` → `Platform` |
| `crates/fstart-codegen/src/linker.rs` | Uses `Platform::linker_arch()` |
| `crates/fstart-codegen/src/stage_gen/mod.rs` | Platform alias, all signatures changed |
| `crates/fstart-codegen/src/stage_gen/tokens.rs` | `halt_expr()` uses alias |
| `crates/fstart-codegen/src/stage_gen/capabilities.rs` | 9 match sites → `Platform` enum |
| `crates/fstart-codegen/src/stage_gen/tests.rs` | Updated for `Platform::Riscv64`, `fstart_platform::` |
| `xtask/src/build_board.rs` | `Platform` methods, sunxi feature decoupled |
| `xtask/src/qemu.rs` | `Platform` parameter, exhaustive match |
| `xtask/src/main.rs` | Passes `config.platform` to qemu |
| `crates/fstart-stage/Cargo.toml` | Decoupled `armv7` from `sunxi` |
| `crates/fstart-soc-sunxi/Cargo.toml` | Removed `fstart-arch` dependency |
| `boards/*/board.ron` (10 files) | `platform:` field → enum variant |

## Git State

Ten commits on `master`:
1. `1b2b71f` — Initial commit: fstart firmware framework with 14 workspace crates
2. `9383113` — Introduce typed Device trait driver model with codegen-produced StageContext
3. `de59c64` — Capability pipeline, BusDevice trait, codegen ordering validation, AArch64 objcopy
4. `187f4af` — Bus support: I2C/SPI/GPIO service traits, DesignWare I2C driver, topological sort
5. `6e6fd82` — Flexible mode: enum dispatch codegen for runtime driver selection
6. (Pending) — Phase 6: FFS + Security — flash layout, crypto, reader, builder, xtask assemble
7. (Pending) — Phase 7: Multi-stage layout — per-stage builds, FFS packaging, bootblock + main
8. (Pending) — Phase 9: Wire capabilities to FFS — real sig verify, stage load, payload load
9. (Pending) — Phase 10: ufmt logging infrastructure — macros, SyncCell, capabilities migration
10. (Pending) — Phase 11: Linux boot verified, GICv3 fix, fstart-log cleanup

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
cargo xtask build --board qemu-riscv64-multi
cargo xtask build --board qemu-riscv64 --release
cargo xtask build --board qemu-riscv64-multi --release

# Assemble FFS images
cargo xtask assemble --board qemu-riscv64
cargo xtask assemble --board qemu-riscv64-multi

# Run on QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64
cargo xtask run --board qemu-riscv64-multi  # boots bootblock stage
```
