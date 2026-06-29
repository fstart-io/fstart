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


### Formatting
Default `rustfmt` (no `rustfmt.toml`). 4-space indent. Edition 2021.
