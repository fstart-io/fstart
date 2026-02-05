# fstart Continuation Plan

Status as of 2026-02-05 (updated Phase 5 complete). This document captures
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

### Verified Working

| Board | Mode | Output |
|-------|------|--------|
| qemu-riscv64 debug | `cargo xtask run --board qemu-riscv64` | `[fstart] uart0: ns16550 console ready` + `[fstart] all capabilities complete` |
| qemu-riscv64 release | `cargo xtask build --board qemu-riscv64 --release` | Builds clean |
| qemu-aarch64 debug | `cargo xtask run --board qemu-aarch64` | `[fstart] uart0: pl011 console ready` + `[fstart] all capabilities complete` |
| qemu-aarch64 release | `cargo xtask build --board qemu-aarch64 --release` | Builds clean |
| qemu-riscv64-flex debug | `cargo xtask run --board qemu-riscv64-flex` | `[fstart] uart0: ns16550 console ready` + `[fstart] all capabilities complete` |
| clippy | `cargo clippy --workspace --exclude fstart-stage -- -D warnings` | Clean |
| fmt | `cargo fmt --all -- --check` | Clean |
| tests | `cargo test --workspace --exclude fstart-stage --exclude fstart-runtime --exclude fstart-platform-*` | 29 pass |

---

## What Remains

### Explore crabtime (Optional)

Investigate `crabtime` (Zig comptime-like macros for Rust) as an
alternative or complement to `build.rs` codegen for enum dispatch
generation. Low priority — current codegen approach works well.

---

### Phase 6: Firmware Filesystem + Security

**Goal**: Implement the full FFS + signature verification chain.

#### 6.1 fstart-crypto

- SHA-256 implementation (or pull in a `no_std` crate like `sha2`).
- SHA3-256 implementation (or `sha3` crate).
- Ed25519 signature verification (or `ed25519-dalek` with `no_std`).
- ECDSA P-256 verification (or `p256` crate).
- All behind feature flags for algorithm agility.

#### 6.2 fstart-ffs Reader (no_std)

- `postcard`-deserialize the `SignedManifest` from a byte slice.
- Verify manifest signature against embedded public key.
- Look up files by name, verify their digests.
- Read file contents by offset/size from the FFS blob.

#### 6.3 fstart-ffs Builder (std, xtask)

- `xtask assemble` command: reads board RON + file list, compresses
  payloads, computes digests, builds manifest, signs with private key.
- Produces a single FFS binary blob.

#### 6.4 Wire Into Capabilities

- `SigVerify`: reads FFS from flash/block device, verifies manifest
  signature, checks file digests. Halts on failure.
- `PayloadLoad`: reads payload from verified FFS, copies to RAM, jumps.
- `StageLoad`: reads next stage from verified FFS, copies to load_addr,
  jumps.

---

### Phase 7: Multi-Stage Layout

**Goal**: Support bootblock (XIP from ROM) + main stage (loaded to RAM).

- `StageLayout::MultiStage` with separate build for each stage.
- Bootblock: XIP, minimal (ConsoleInit + SigVerify + StageLoad).
- Main stage: RAM, full (MemoryInit + DriverInit + FdtPrepare +
  PayloadLoad).
- `xtask build` builds all stages and assembles into FFS.
- Linker script generation already supports XIP vs RAM layouts.

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

### Phase 9: Polish + CI

- Logging infrastructure (`fstart-log`): structured log levels, compile-time
  filtering.
- Allocator (`fstart-alloc`): bump allocator for stages that need heap.
- CI pipeline: GitHub Actions with `cargo check`, `clippy`, `fmt`, `test`,
  cross-build both boards.
- Measured boot hooks: TPM event log placeholder.
- More drivers: SiFive UART, VirtIO block, etc.

---

## File Summary (Phase 5 Changes)

| File | Change |
|------|--------|
| `crates/fstart-codegen/src/stage_gen.rs` | `SERVICE_TRAITS` metadata, `generate_flexible_enums()`, flexible-mode branching in all generate functions, `generate_flexible_wrapping()`, two-phase construction, 11 new tests (29 total) |
| `boards/qemu-riscv64-flex/board.ron` | **New**: Flexible-mode board for QEMU RISC-V 64 |
| `docs/continuation-plan.md` | Updated with Phase 5 completion |

## Git State

Five commits on `master`:
1. `1b2b71f` — Initial commit: fstart firmware framework with 14 workspace crates
2. `9383113` — Introduce typed Device trait driver model with codegen-produced StageContext
3. `de59c64` — Capability pipeline, BusDevice trait, codegen ordering validation, AArch64 objcopy
4. `187f4af` — Bus support: I2C/SPI/GPIO service traits, DesignWare I2C driver, topological sort
5. (Pending) — Phase 5: Flexible mode — enum dispatch codegen

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
cargo xtask build --board qemu-riscv64 --release

# Run on QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64
cargo xtask run --board qemu-riscv64-flex
```
