# fstart Continuation Plan

Status as of 2026-02-05. This document captures what has been built, what
remains, and the recommended order of work for future sessions.

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

### Infrastructure Fixes

- **AArch64 objcopy**: `xtask build` now automatically runs `llvm-objcopy
  -O binary` for AArch64 boards (QEMU `-bios` expects flat binary, not ELF).
- **`cargo xtask run --board qemu-aarch64`** now works end-to-end.

### Verified Working

| Board | Mode | Output |
|-------|------|--------|
| qemu-riscv64 debug | `cargo xtask run --board qemu-riscv64` | `[fstart] uart0: ns16550 console ready` + `[fstart] all capabilities complete` |
| qemu-riscv64 release | `cargo xtask build --board qemu-riscv64 --release` | Builds clean |
| qemu-aarch64 debug | `cargo xtask run --board qemu-aarch64` | `[fstart] uart0: pl011 console ready` + `[fstart] all capabilities complete` |
| qemu-aarch64 release | `cargo xtask build --board qemu-aarch64 --release` | Builds clean |
| clippy | `cargo clippy --workspace --exclude fstart-stage -- -D warnings` | Clean |
| fmt | `cargo fmt --all -- --check` | Clean |
| tests | `cargo test --workspace --exclude fstart-stage --exclude fstart-runtime --exclude fstart-platform-*` | 8 pass |

---

## What Remains

### Next Priority: Phase 4 — Bus Support (Driver Model Phase 4)

**Goal**: Enable bus-attached devices (I2C sensors, SPI flash, GPIO
expanders) with parent-before-child init ordering.

#### 4.1 Bus Service Traits

Add to `fstart-services/src/`:

```rust
// i2c.rs
pub trait I2cBus: Send + Sync {
    fn read(&self, addr: u8, reg: u8, buf: &mut [u8]) -> Result<usize, ServiceError>;
    fn write(&self, addr: u8, reg: u8, data: &[u8]) -> Result<usize, ServiceError>;
}

// spi.rs
pub trait SpiBus: Send + Sync {
    fn transfer(&self, cs: u8, tx: &[u8], rx: &mut [u8]) -> Result<usize, ServiceError>;
}

// gpio.rs
pub trait GpioController: Send + Sync {
    fn get(&self, pin: u32) -> Result<bool, ServiceError>;
    fn set(&self, pin: u32, value: bool) -> Result<(), ServiceError>;
    fn set_direction(&self, pin: u32, output: bool) -> Result<(), ServiceError>;
}
```

#### 4.2 Topological Sort in Codegen

Update `stage_gen.rs`:
- Sort devices by dependency: root devices first, then children.
- Validate that every `parent` reference names an existing device.
- Validate that the parent device provides a bus service.
- Generate parent-before-child init ordering.
- Pass parent bus reference to child device constructors.

#### 4.3 First Bus Driver

Implement DesignWare I2C controller as `fstart-drivers/src/i2c/designware.rs`:
- `register_structs!` / `register_bitfields!` for DW APB I2C registers.
- `DesignwareI2cConfig { base_addr, clock_freq, bus_speed }`.
- `impl Device for DesignwareI2c`.
- `impl I2cBus for DesignwareI2c`.

#### 4.4 Test Board

Create `boards/qemu-riscv64-i2c/board.ron` (or extend existing) with a
bus hierarchy for integration testing.

---

### Phase 5: Flexible Mode (Driver Model Phase 5)

**Goal**: Support runtime driver selection via enum dispatch (no trait
objects, no alloc).

#### 5.1 Enum Dispatch Codegen

When `mode: Flexible`, codegen generates:

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

#### 5.2 Explore crabtime

Investigate `crabtime` (Zig comptime-like macros for Rust) as an
alternative to `build.rs` codegen for enum dispatch generation.

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

## File Summary (Modified in This Session)

| File | Change |
|------|--------|
| `crates/fstart-services/src/device.rs` | Added `BusDevice` trait |
| `crates/fstart-services/src/lib.rs` | Re-export `BusDevice` |
| `crates/fstart-capabilities/src/lib.rs` | All 7 capability functions: `console_ready`, `memory_init`, `driver_init_complete`, `sig_verify`, `fdt_prepare`, `payload_load`, `stage_load` + `write_usize` helper |
| `crates/fstart-codegen/src/stage_gen.rs` | Full capability pipeline codegen, ordering validation, 8 unit tests |
| `crates/fstart-codegen/Cargo.toml` | Added `heapless` dev-dependency for tests |
| `xtask/src/build_board.rs` | Auto `llvm-objcopy -O binary` for AArch64 |
| `docs/continuation-plan.md` | This document |

## Git State

Three commits expected on `master`:
1. `1b2b71f` — Initial commit: fstart firmware framework with 14 workspace crates
2. `9383113` — Introduce typed Device trait driver model with codegen-produced StageContext
3. (Pending) — Capability pipeline, BusDevice trait, codegen ordering validation, AArch64 objcopy fix

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
cargo xtask build --board qemu-riscv64 --release

# Run on QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64
```
