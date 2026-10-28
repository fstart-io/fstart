# Native Rust VGA option-ROM execution

`fstart-driver-vbios` uses rsemu's `no_std + alloc` x86 interpreter. The
vendored C x86emu, compatibility headers, C shim, C build script and backend
selection features have been removed. The normal `google-vilboz` build uses
Rust; no special board variant or fallback backend is required.

## Library and platform boundary

The dependency is pinned to the [firmware embedding fork](https://github.com/ArthurHeymans/rsemu/tree/firmware-embedding),
revision `8c1a67b8fca10c402398b781ddd118006c3ac88c`, based on upstream
`72aea81db5ce726653e140c7e0c5bacdba8a3b62`. No local path override is needed.
The fork's [`cpu::x86::embedding` API](https://github.com/ArthurHeymans/rsemu/blob/8c1a67b8fca10c402398b781ddd118006c3ac88c/docs/cpu/x86-embedding.md)
returns faults and software interrupts, bounds REP iterations, and provides
optional CPUID/MSR/TSC/cache-operation exits for firmware consumers. It never
implicitly executes privileged operations on the host.

`crates/driver-vbios/src/native.rs` adapts that library to the existing driver
policy. The driver retains private guest RAM, authenticated ROM validation,
GPU aperture bounds, MMIO/PIO helpers, PCI BIOS services, execution deadlines
and VBE mode selection. There is no second PCI/ECAM implementation.

The synchronous ACTIVE/BUSY ownership model remains private to the adapter.
No context reference survives an interpreter call; memory/I/O callbacks must
not re-enter the CPU while its execution lock is held. Refused accesses are
sticky and stop execution. Mixed RAM/MMIO accesses and wide MMIO fail closed;
wide private-RAM operands do not become invented hardware transactions.

## Build and validation

```sh
cargo test -p fstart-driver-vbios
cargo check -p fstart-driver-vbios --target x86_64-unknown-none
cargo clippy -p fstart-driver-vbios -- -D warnings
cargo fbuild assemble --board google-vilboz --release --payload uefi \
    --donor /path/to/vilboz-backup.rom
```

The driver regression covers POST/far return, repeated INT10/IRET, an ES:DI
VBE signature write, ROM checks and memory rejection. The library fork adds
seven embedding tests, including REP progress, I/O widths, fault exits and
LDMXCSR/STMXCSR with CR0.MP clear; the existing 311 selected x86 tests also
passed. These do not substitute for external instruction-conformance corpora.

The earlier opt-in Rust adapter completed two Vilboz cold boots: four CPUs,
Raven2 POST, VBE 3.0 with 20 modes, mode `0x1d4` at 1366x768/32 bpp with
1408-pixel stride at `0xe0000000`, and successful CrabEFI initialization.
A preceding boot hit the existing AP-release #GP before VBIOS execution.
The Rust image was verified by a complete flash readback. These observations
precede the final C removal and dependency pinning; the default-backend build
still needs a fresh hardware validation. They do not prove OS boot, visual
panel output, persistent UEFI variables or transaction-by-transaction parity.

## Remaining scope

This is a VGA ROM driver, not yet a Rust Blobolator/FSP harness. The shared CPU
exits support such a consumer, but AMD passthrough policy, call-chain tracing
and transaction replay belong above the library. The VBIOS driver uses the
implemented virtual CPU feature set, not unsupported host CPUID features.
AVX/XSAVE and newer instructions are not supplied. New instruction support
belongs in rsemu's shared decoder/interpreter and its regression suite, not in
an AMD-only second emulator.
