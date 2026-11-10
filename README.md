<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logos/fstart-dark.svg">
    <img src="docs/assets/logos/fstart.svg" width="420" alt="fstart — PCB crab logo">
  </picture>
</p>

# fstart

A firmware framework in Rust using Rust board crates with builder-pattern board
metadata, fixed handwritten stage flow, and step-based hardware initialization.

Supports RISC-V 64, AArch64, ARMv7 and x86. Boots Linux and UEFI.

**Experimental.** This is early-stage software. Internals, APIs, and the board
file format may change drastically without notice.

## Quick start

```bash
# Run a pre-defined board in QEMU
cargo fbuild run --board qemu-riscv64
cargo fbuild run --board qemu-aarch64

# Build without running
cargo fbuild build --board qemu-riscv64

# Build a signed firmware image (FFS). Without --signing-key the image is
# signed with a fresh throwaway key, discarded after the build.
openssl genpkey -algorithm ed25519 -out signing.pem
cargo fbuild assemble --board qemu-riscv64 --signing-key signing.pem
```
