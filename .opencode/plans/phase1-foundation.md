# Phase 1: Foundation Types + Build System — COMPLETED

## Key Design Decision: No Separate Stage Crates

The original plan had `stages/stage-monolithic/`, `stages/stage-bootblock/`, `stages/stage-main/` as separate hand-written crates. **This was wrong.**

Since the RON file defines which capabilities run in which order, the stage binary should be entirely **generated** from the RON. There is a single `crates/fstart-stage/` crate whose `build.rs` reads the board RON (via `FSTART_BOARD_RON` env var) and generates:

1. **`generated_stage.rs`** — The `fstart_main()` entry point with driver init and capability calls
2. **`link.ld`** — Linker script generated from the memory map in the RON

The `main.rs` simply does: `include!(concat!(env!("OUT_DIR"), "/generated_stage.rs"));`

`xtask` orchestrates the build by setting env vars and cargo features based on the board RON.

## What Was Built

- **14 workspace crates**, all compiling cleanly
- **2 board definitions** (qemu-riscv64, qemu-aarch64) 
- **Both boards cross-compile successfully** to their respective targets
- **Generated code is correct**: driver instantiation and capability sequence match the RON exactly

## Verified

```
cargo check --workspace --exclude fstart-stage  # ✅ clean (host crates)
cargo xtask build --board qemu-riscv64           # ✅ cross-compiles to riscv64gc
cargo xtask build --board qemu-aarch64           # ✅ cross-compiles to aarch64
```
