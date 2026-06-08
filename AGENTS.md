# AGENTS.md — fstart firmware framework

## Project Overview

fstart is a next-generation firmware framework in Rust. The current architecture
direction is Rust board crates with builder-pattern board metadata, fixed
handwritten stage flow, and step-based hardware initialization. The old
RON-driven generated-stage design is deprecated.

For domain inspiration, reference codebases are available at `~/src/coreboot` (C,
payload/stage architecture) and `~/src/u-boot` (C, device-tree-driven board defs).

## Design Documents

- **[Rust Board Builder and Fixed Stage Flow Plan](docs/rust-board-builder-stage-flow-plan.md)** — current architecture direction. It supersedes the previous RON/stage-codegen design docs.

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
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 \
    --exclude fstart-platform-armv7 --exclude fstart-runtime

# Build a specific board (current implementation still uses transitional RON/codegen;
# new work should follow the Rust board-builder plan)
cargo xtask build --board qemu-riscv64
cargo xtask build --board qemu-aarch64
cargo xtask build --board qemu-armv7
cargo xtask build --board bananapi-m1
cargo xtask build --board qemu-riscv64 --release

# Build and launch in QEMU
cargo xtask run --board qemu-riscv64
cargo xtask run --board qemu-aarch64
cargo xtask run --board qemu-armv7

# Clippy — host crates only (fstart-stage and platform crates need cross-compile)
cargo clippy --workspace --exclude fstart-stage \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 \
    --exclude fstart-platform-armv7 --exclude fstart-runtime -- -D warnings

# Format
cargo fmt --all
cargo fmt --all -- --check   # CI-style check

# Run tests (47 codegen + 14 FFS; add more with #[cfg(test)])
cargo test --workspace --exclude fstart-stage --exclude fstart-runtime \
    --exclude fstart-alloc \
    --exclude fstart-platform-riscv64 --exclude fstart-platform-aarch64 \
    --exclude fstart-platform-armv7

# Run a single test by name
cargo test --package fstart-types -- test_name_here

# Run a single test file (integration test)
cargo test --package fstart-codegen --test integration_test_name
```

Note: `fstart-stage`, `fstart-runtime`, and platform crates are `no_std` `#![no_main]`
binaries — they cannot be tested with `cargo test` on the host. Test logic for these
via `fstart-types` or `fstart-codegen` (which are `std`-capable).

## Workspace Layout (22 crates)

### Host-side (std) crates

| Crate | Purpose |
|---|---|
| `xtask` | Build orchestrator, QEMU launcher, eGON patching |
| `fstart-codegen` | Transitional RON→Rust codegen and linker script gen (to be replaced by the Rust board-builder/fixed-flow plan) |
| `fstart-device-registry` | Transitional aggregation of driver `Config` types for codegen/dynamic matching |

### Shared crates (std feature for host, no_std for target)

| Crate | Purpose |
|---|---|
| `fstart-types` | `BoardConfig`, `MemoryMap`, `StageLayout`, all shared types |
| `fstart-ffs` | Firmware filesystem reader/builder |

### Target (no_std) crates — core infrastructure

| Crate | Purpose |
|---|---|
| `fstart-stage` | Final binary — `include!`s generated code |
| `fstart-runtime` | `#[panic_handler]` |
| `fstart-services` | Trait defs: `Console`, `BlockDevice`, `Timer`, `Device`, `BusDevice` |
| `fstart-capabilities` | Capability impls (ConsoleInit, DramInit, PayloadLoad, etc.) |
| `fstart-arch` | Architecture utils: `udelay`, `sdelay`, `mdelay`, `halt` (feature-gated: `armv7`, `aarch64`, `riscv64`) |
| `fstart-log` | Logging macros (`info!`, `error!`, etc.) backed by ufmt |
| `fstart-mmio` | MMIO register access helpers |
| `fstart-crypto` | Signature verify, hashing |
| `fstart-alloc` | Allocator (skeleton) |

### Target (no_std) crates — platform / SoC

| Crate | Purpose |
|---|---|
| `fstart-platform-riscv64` | `_start` entry for RISC-V 64 |
| `fstart-platform-aarch64` | `_start` entry for AArch64 |
| `fstart-platform-armv7` | `_start` entry for ARMv7 (optional `sunxi` feature) |
| `fstart-soc-sunxi` | Allwinner eGON boot header, FEL support, boot media detection |

### Target (no_std) crates — individual drivers

| Crate | Driver | Services |
|---|---|---|
| `fstart-driver-ns16550` | NS16550(A) UART | `Console` |
| `fstart-driver-pl011` | ARM PL011 UART | `Console` |
| `fstart-driver-designware-i2c` | DesignWare APB I2C | `I2cBus` |
| `fstart-driver-sunxi-ccu` | Allwinner A20 CCU | `ClockController` |
| `fstart-driver-sunxi-a20-dramc` | Allwinner A20 DRAM controller | `MemoryController` |
| `fstart-driver-sunxi-mmc` | Allwinner A20 SD/MMC | `BlockDevice` |
| `fstart-sunxi-ccu-regs` | Shared CCU register defs (used by sunxi drivers) | — |

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
- `#![no_std]` everywhere except `xtask`, `fstart-codegen`, and `fstart-device-registry`
- Bounded containers only: `heapless::Vec<T, N>`, `HString<N>` — never `alloc::Vec`
  in firmware crates
- MMIO registers: use the `tock-registers` crate (`register_structs!`, `register_bitfields!`)
  for all new drivers — never raw `read_volatile`/`write_volatile`
- `unsafe impl Send + Sync` on MMIO driver structs with a `// SAFETY:` comment
- Driver construction remains typed; long-term hardware initialization should use
  step-based `HardwareInit` methods with default no-ops instead of a single
  catch-all `Device::init()` lifecycle.
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

### Driver / Hardware Init Pattern
Every driver struct:
1. Lives in its own crate `fstart-driver-<name>/src/lib.rs`.
2. Defines registers with `register_structs!` / `register_bitfields!` (tock-registers).
3. Stores `regs: &'static <Regs>` constructed from a typed config/address in `new()`.
4. Defines a typed `Config`/builder with only the fields it needs.
5. Implements the service traits it provides (`Console`, `BlockDevice`, `Timer`, etc.).
6. Implements step-based `HardwareInit` methods only for the phases where it
   participates; default no-op methods are preferred over a single catch-all
   `init()`.
7. Spin-waits use `core::hint::spin_loop()`.

See [docs/rust-board-builder-stage-flow-plan.md](docs/rust-board-builder-stage-flow-plan.md)
for the current architecture direction.

### Board Authoring Direction
- Mainboards should become normal Rust board crates under `boards/<board-name>/`.
- Use fluent builder-pattern metadata instead of RON for board information.
- Keep mainboard code light: describe topology/configuration and board-specific
  hook devices; generic phase behavior belongs in hardware init crates.
- Fixed handwritten stage flow should call feature-gated init steps over the
  device graph.
- A separate dynamic board-blob mode may serialize the same builder metadata and
  attach it to a generic stage at build/packaging time.

## Feature Flags

Features should be selected from board crate metadata/build mode and stage flow
requirements:
- platform features (`riscv64`, `aarch64`, `armv7`, `x86_64`) select platform crates.
- driver features select compiled-in driver crates/registry variants.
- flow features select fixed stage-flow steps.
- `std` on shared crates is used by host-side tools only.

## Known IDE Issues (Not Real Errors)

- `fstart-runtime` conflicts with std's `panic_handler` when checked as host target.
  This is expected for `no_std` crates.

## FIT (Flattened Image Tree) Payload Support

FIT images (U-Boot's `.itb` format) bundle kernel, ramdisk, FDT, and firmware
into a single DTB-format blob with SHA-256 hash integrity and configuration
selection. The `fstart-fit` crate parses FIT images and runs identically at
buildtime (xtask, `std`) and runtime (firmware, `no_std`) — zero code duplication.

### Two Parse Modes

- **Buildtime**: xtask reads the `.itb`, extracts kernel/ramdisk/fdt as separate
  FFS entries. Firmware loads them like LinuxBoot. The FIT parser runs in xtask
  at assembly time.
- **Runtime**: the whole `.itb` is embedded in FFS. Firmware parses the FIT
  in-place (zero-copy on memory-mapped flash) and copies each component to its
  load address from the FIT metadata.

### Building FIT Images

FIT images are built from `.its` (Image Tree Source) files using `mkimage` (from
u-boot-tools) and `dtc` (device tree compiler). Templates are in `fit/`.

```bash
# Prerequisites: Linux kernel and u-root initramfs
# See "Building Test Payloads" below.

# Build FIT for riscv64
nix-shell -p ubootTools dtc --run "mkimage -f fit/qemu-riscv64.its fit/qemu-riscv64.itb"

# Build FIT for aarch64
nix-shell -p ubootTools dtc --run "mkimage -f fit/qemu-aarch64.its fit/qemu-aarch64.itb"
```

### Builder-style FIT metadata sketch

```rust
Payload::fit_image("../../fit/qemu-riscv64.itb")
    .default_config()
    .parse_buildtime() // or .parse_runtime()
    .platform_fdt(0x87F0_0000)
    .bootargs("console=ttyS0 earlycon=sbi")
    .opensbi("fw_dynamic.bin", 0x8010_0000)
```

## Building Test Payloads

External repositories are used to build Linux kernels and initramfs images for
testing FIT payloads in QEMU.

### u-root initramfs (Go-based, at `~/src/u-root`)

u-root builds lightweight Go initramfs images with standard Linux tools (ls, cat,
init, shell, kexec, etc.). It also contains a native FIT parser and `fitboot`
command in Go.

```bash
# Install the u-root tool
cd ~/src/u-root && go install

# Build riscv64 initramfs
GOARCH=riscv64 GOOS=linux GORISCV64=rva22u64 \
    u-root -o fit/initramfs-riscv64 core

# Build aarch64 initramfs
GOARCH=arm64 GOOS=linux \
    u-root -o fit/initramfs-aarch64 core
```

### Linux kernel (at `~/src/linux`)

Minimal kernel configs for QEMU virt machines are provided by u-root in
`~/src/u-root/configs/`.

```bash
# riscv64 kernel
cd ~/src/linux
nix-shell -p gcc14 flex bison bc perl --run "
    export CROSS_COMPILE=riscv64-unknown-linux-gnu-
    export ARCH=riscv
    make mrproper
    make tinyconfig
    cat ~/src/u-root/configs/riscv64_config.txt \
        ~/src/u-root/configs/generic_config.txt >> .config
    make olddefconfig
    make -j\$(($(nproc) * 2 + 1))
"
cp ~/src/linux/arch/riscv/boot/Image fit/Image-riscv64

# aarch64 kernel
cd ~/src/linux
nix-shell -p gcc14 flex bison bc perl --run "
    export CROSS_COMPILE=aarch64-unknown-linux-gnu-
    export ARCH=arm64
    make mrproper
    make tinyconfig
    cat ~/src/u-root/configs/arm64_config.txt \
        ~/src/u-root/configs/generic_config.txt >> .config
    make olddefconfig
    make -j\$(($(nproc) * 2 + 1))
"
cp ~/src/linux/arch/arm64/boot/Image fit/Image-aarch64
```

### Full FIT test workflow

```bash
# 1. Build initramfs (both architectures)
cd ~/src/u-root && go install
GOARCH=riscv64 GOOS=linux u-root -o ~/src/fstart_ParseFITBuildtime/fit/initramfs-riscv64 core
GOARCH=arm64 GOOS=linux u-root -o ~/src/fstart_ParseFITBuildtime/fit/initramfs-aarch64 core

# 2. Build Linux kernels and copy to fit/
# (see above)

# 3. Build FIT images
cd ~/src/fstart_ParseFITBuildtime
nix-shell -p ubootTools dtc --run "mkimage -f fit/qemu-riscv64.its fit/qemu-riscv64.itb"
nix-shell -p ubootTools dtc --run "mkimage -f fit/qemu-aarch64.its fit/qemu-aarch64.itb"

# 4. Assemble and run with fstart
cargo xtask assemble --board qemu-riscv64
cargo xtask run --board qemu-riscv64
```

### Standalone QEMU test (without fstart, to verify kernel+initramfs work)

```bash
nix-shell -p qemu --run "
    qemu-system-riscv64 -M virt -cpu rv64 -m 1G -nographic \
        -kernel fit/Image-riscv64 \
        -initrd fit/initramfs-riscv64 \
        -append 'earlycon=sbi console=ttyS0'
"

nix-shell -p qemu --run "
    qemu-system-aarch64 -M virt -cpu cortex-a57 -m 1G -nographic \
        -kernel fit/Image-aarch64 \
        -initrd fit/initramfs-aarch64 \
        -append 'console=ttyAMA0'
"
```

## What Not to Do

- Do NOT add `alloc` to firmware crates without explicit discussion
- Do NOT use `std` in any crate under `fstart-stage`'s dependency tree
- Do NOT reintroduce new RON/stage-codegen design docs; use the Rust board-builder/fixed-flow plan
- Do NOT use `naked_functions` feature attribute (stabilized since rustc 1.88)
- Do NOT use `[u8; 64]` in serde structs — split to `[u8; 32]` halves instead
