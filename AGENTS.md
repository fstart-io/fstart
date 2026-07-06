# AGENTS.md — fstart firmware framework

## Project Overview

fstart is a next-generation firmware framework in Rust. The current architecture
direction is Rust board crates with builder-pattern board metadata, fixed
handwritten stage flow, and step-based hardware initialization. The old
RON-driven generated-stage design is deprecated.

For domain inspiration, reference codebases are available at `~/src/coreboot` (C,
payload/stage architecture) and `~/src/u-boot` (C, device-tree-driven board defs).

## Design Documents

- **[fstart Architecture: Config as Data, Fixed Family Flows, Few Crates](docs/architecture.md)** — the plan of record. It supersedes the earlier board-builder/stage-flow and BSP/platform-recipe plans, and consolidates the fstart-new reboot sketch.

## Environment

This is a NixOS system. Tools not on `$PATH` (e.g., `qemu`, `file`, `objdump`) must
be run via `nix-shell`:

```bash
nix-shell -p qemu file --run "qemu-system-riscv64 -M virt -bios firmware.bin"
nix-shell -p binutils --run "objdump -d target/.../fstart-stage"
```

### Formatting

Default `rustfmt` (no `rustfmt.toml`). 4-space indent. Edition 2021.

## BREAKING changes

This is a grassroots projects. Breaking changes are expected everywhere.
We want no backwards compatibility or safe migrations to new architectural designs.

## Scaling

Where it put some code is very important.
The goal of this project is to scale to 100's of platforms and 1000's of boards.
Having the same boilerplate in each board dir is not an option.
Make sure to have proper abstractions: we have driver code that implements actual hardware init parts.
Then we have "platforms" that coordinates the driver code in both early simple flows and later ones.
Some platform code and structure can be shared accross all for instance Intel platforms. Some is platform specific like gm965/ich8.
Some code is board level specific.

If you don't know where to put things or have some doubts on structure or reuse. ASK the user unless prompted otherwise.
