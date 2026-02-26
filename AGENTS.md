# AGENTS.md — fstart firmware framework

## Project Overview

fstart is a next-generation firmware framework in Rust. Board `.ron` files are the
single source of truth — the build system reads them and generates stage entry points,
driver instantiation, and linker scripts. No hand-written stage code.

For domain inspiration, reference codebases are available at `~/src/coreboot` (C,
payload/stage architecture) and `~/src/u-boot` (C, device-tree-driven board defs).

## Design Documents

- **[Driver Model](docs/driver-model.md)** — typed device/driver architecture inspired
  by coreboot's device tree and U-Boot's uclass/ops model, redesigned for Rust's type
  system. Covers the `Device` trait, associated `Config` types, codegen-produced
  `Devices`/`StageContext` structs, bus hierarchies, and Rigid vs Flexible dispatch.
- **[Continuation Plan](docs/continuation-plan.md)** — what has been built, what
  remains, and the recommended order of work. Includes phase-by-phase status and
  detailed next-step descriptions.

## Environment

This is a NixOS system. Tools not on `$PATH` (e.g., `qemu`, `file`, `objdump`) must
be run via `nix-shell`:

```bash
nix-shell -p qemu file --run "qemu-system-riscv64 -M virt -bios firmware.bin"
nix-shell -p binutils --run "objdump -d target/.../fstart-stage"
```

## Build / Run / Check Commands

```bash
# Check all host-side crates (fast, no cross-compile env needed)
cargo check --workspace --exclude fstart-stage \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64

# Build a specific board (sets FSTART_BOARD_RON, cross-compiles with -Z build-std=core)
cargo xtask build --board qemu-riscv64
cargo xtask build --board qemu-aarch64
cargo xtask build --board qemu-riscv64 --release

# Build and launch in QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64

# Clippy — host crates only (fstart-stage and platform crates need cross-compile)
cargo clippy --workspace --exclude fstart-stage \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 -- -D warnings

# Format
cargo fmt --all
cargo fmt --all -- --check   # CI-style check

# Run tests (8 codegen unit tests; add more with #[cfg(test)])
cargo test --workspace --exclude fstart-stage --exclude fstart-runtime \
    --exclude fstart-alloc \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64

# Run a single test by name
cargo test --package fstart-types -- test_name_here

# Run a single test file (integration test)
cargo test --package fstart-codegen --test integration_test_name
```

Note: `fstart-stage`, `fstart-runtime`, and platform crates are `no_std` `#![no_main]`
binaries — they cannot be tested with `cargo test` on the host. Test logic for these
via `fstart-types` or `fstart-codegen` (which are `std`-capable).

## Workspace Layout (14 crates)

| Crate | Runs on | Purpose |
|---|---|---|
| `xtask` | host | Build orchestrator, QEMU launcher |
| `fstart-codegen` | host/build.rs | RON→Rust codegen, linker script gen |
| `fstart-types` | both (`std` feature) | `BoardConfig`, `MemoryMap`, all shared types |
| `fstart-ffs` | both (`std` feature) | Firmware filesystem reader/builder |
| `fstart-stage` | target | Final binary — `include!`s generated code |
| `fstart-runtime` | target | `#[panic_handler]` |
| `fstart-services` | target | Trait defs: `Console`, `BlockDevice`, `Timer`, `Device`, `BusDevice` |
| `fstart-drivers` | target | Driver impls (feature-gated: `ns16550`, `pl011`) |
| `fstart-capabilities` | target | `StageContext`, capability impls |
| `fstart-crypto` | target | Signature verify, hashing (skeleton) |
| `fstart-alloc` | target | Allocator (skeleton) |
| `fstart-log` | target | Logging (skeleton) |
| `fstart-platform-riscv64` | target | `_start` entry, `halt()` |
| `fstart-platform-aarch64` | target | `_start` entry, `halt()` |

## Code Style

### Formatting
Default `rustfmt` (no `rustfmt.toml`). 4-space indent. Edition 2021.

### Imports — use this order, with blank line between groups
```rust
// 1. External crates (core, alloc, third-party)
use core::ptr;
use heapless::String as HString;
use serde::{Deserialize, Serialize};

// 2. Workspace crate imports
use fstart_services::{Console, ServiceError};
use fstart_types::device::Resources;

// 3. Crate-local imports
use crate::memory::MemoryMap;
use crate::stage::StageLayout;
```

### Naming
- **Crates**: `fstart-<component>` (hyphenated)
- **Modules**: `snake_case` (`ron_loader`, `stage_gen`)
- **Types/Traits**: `PascalCase` (`BoardConfig`, `Ns16550`, `Console`)
- **Constants**: `SCREAMING_SNAKE_CASE` (`LSR_DATA_READY`, `FFS_MAGIC`)
- **Functions**: `snake_case` (`from_resources`, `generate_stage_source`)
- **Heapless strings**: always alias `use heapless::String as HString`

### Type Conventions
- `#![no_std]` everywhere except `xtask` and `fstart-codegen`
- Bounded containers only: `heapless::Vec<T, N>`, `HString<N>` — never `alloc::Vec`
  in firmware crates
- MMIO registers: use the `tock-registers` crate (`register_structs!`, `register_bitfields!`)
  for all new drivers — never raw `read_volatile`/`write_volatile`
- `unsafe impl Send + Sync` on MMIO driver structs with a `// SAFETY:` comment
- Drivers implement the `Device` trait with `type Config`, `fn new(&Config)`, `fn init()`
- Serde derives on all config types: `#[derive(Debug, Clone, Serialize, Deserialize)]`
- Enums also derive `Copy, PartialEq, Eq` when small/fieldless

### Error Handling
| Context | Pattern |
|---|---|
| Host tools (xtask) | `Result<T, String>` with `.map_err(\|e\| format!(...))` |
| `no_std` services | `Result<T, ServiceError>` (enum: `Timeout`, `HardwareError`, …) |
| Drivers | `Result<Self, DeviceError>` for construction (`MissingResource`, `InitFailed`) |
| `build.rs` | `unwrap_or_else(\|_\| panic!("..."))` |
| Codegen errors | Emit `compile_error!("...")` in generated source |

Never use `.unwrap()` silently in firmware code. In host-side code, prefer
`.map_err()` over `.unwrap()`.

### Doc Comments
- `//!` module-level doc on every `lib.rs` and significant modules
- `///` on every public struct, enum, trait, and function
- Inline `//` comments for register offsets, bit flags, and non-obvious logic
- `// SAFETY:` before every `unsafe` block

### Driver Pattern
Every driver struct:
1. Lives in `fstart-drivers/src/<category>/<name>.rs` (feature-gated)
2. Defines registers with `register_structs!` / `register_bitfields!` (tock-registers)
3. Stores `regs: &'static <Regs>` constructed from base address in `new()`
4. Defines a typed `Config` struct (e.g., `Ns16550Config`) with only the fields it needs
5. Implements `Device` trait: `const NAME`, `const COMPATIBLE`, `type Config`,
   `fn new(&Config)`, `fn init()`
6. Implements one or more service traits (`Console`, `BlockDevice`, `Timer`)
7. Spin-waits use `core::hint::spin_loop()`

See [docs/driver-model.md](docs/driver-model.md) for the full architecture.

### Board RON Files
- Located at `boards/<board-name>/board.ron`
- Raw RON tuple syntax `( ... )` — no outer struct wrapper like `Board(...)`
- Deserializes to `fstart_types::board::BoardConfig`
- Always has: `name`, `platform`, `memory`, `devices`, `stages`, `security`, `mode`, `payload`
- Comments use `//` (RON supports them)
- `memory.regions` contains only ROM and RAM — device MMIO addresses go in
  `devices[].resources.mmio_base`, not in the memory map
- `stack_size` is per-stage (in `stages`), not in `memory`

## Architecture: How a Build Works

```
boards/qemu-riscv64/board.ron
  ──► xtask reads RON, determines target triple + features
  ──► cargo build -p fstart-stage --target <triple> --features <feats>
        ──► fstart-stage/build.rs reads $FSTART_BOARD_RON
        ──► calls fstart_codegen to produce:
              • generated_stage.rs  (fstart_main() with driver init sequence)
              • link.ld             (memory regions from RON)
        ──► fstart-stage/src/main.rs does include!(generated_stage.rs)
  ──► final ELF: platform _start → fstart_main → halt
  ──► (AArch64 only) llvm-objcopy -O binary → .bin for QEMU -bios
```

## Feature Flags

Features flow from RON → xtask → `--features` on `fstart-stage`:
- `riscv64` / `aarch64` — selects platform crate (optional dep)
- `ns16550` / `pl011` / `sifive-uart` — enables driver modules
- `std` on `fstart-types` / `fstart-ffs` — used by host-side tools only

## Known IDE Issues (Not Real Errors)

- `fstart-stage` shows `OUT_DIR` / `include!` errors in LSP — build.rs needs
  `FSTART_BOARD_RON` which the IDE doesn't set. Actual builds are clean.
- `fstart-runtime` conflicts with std's `panic_handler` when checked as host target.
  This is expected for `no_std` crates.

## What Not to Do

- Do NOT add `alloc` to firmware crates without explicit discussion
- Do NOT use `std` in any crate under `fstart-stage`'s dependency tree
- Do NOT create separate crates per stage — `fstart-stage` is THE stage binary
- Do NOT wrap board RON in `Board(...)` — use raw tuple `(...)` syntax
- Do NOT use `naked_functions` feature attribute (stabilized since rustc 1.88)
- Do NOT use `[u8; 64]` in serde structs — split to `[u8; 32]` halves instead
