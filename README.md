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

Install [Rust via rustup](https://rustup.rs/) and QEMU (on macOS, `brew install qemu`).
Run the commands from the repository root so rustup selects the pinned nightly.
The `halt` payload boots the firmware without kernel or firmware payload files.
Look for `ramstage: ready for payload`, then exit QEMU (`Ctrl-A`, then `X`).

```bash
# Run a pre-defined board in QEMU
cargo fbuild qemu --board qemu-riscv64 --payload halt
cargo fbuild qemu --board qemu-aarch64 --payload halt

# Build without running
cargo fbuild build --board qemu-riscv64 --payload halt

# Boot the tests a board's plan declares, as CI does, with the guests and
# firmware Nix builds for them
nix build .#boot-assets
cargo fbuild boot-test --assets result qemu-q35

# Build a signed firmware image (FFS). Without --signing-key the image is
# signed with a fresh throwaway key, discarded after the build.
openssl genpkey -algorithm ed25519 -out signing.pem
cargo fbuild assemble --board qemu-riscv64 --payload halt --signing-key signing.pem
```

`fbuild` runs on the host. If Cargo tries to compile it for `wasm32-wasip1`,
check for `CARGO_BUILD_TARGET` or a Cargo `build.target` override and remove it
for this checkout. The QEMU quick start does not use a WebAssembly target.
Building x86 firmware still requires x86 ELF tools; the macOS quick start above
uses RISC-V or AArch64 firmware instead.
