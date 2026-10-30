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

On 2026-10-30 the final Rust-only revision
`e2aa8a6738f2758b44ed508c07edfb707b315a3b` was built with `--payload uefi`
and tested on Vilboz. Two fresh full-flash recovery reads matched before
programming; the programmed image also matched an explicit full readback.
Its SHA-256 was
`f50acd0ce98952e325cb37eda814a60bceaa5f0a3a9dcfbc0eb2dfd2f2974db7`.
The linked ELF had no C emulator or legacy C-ABI emulator symbols.

Both a G3/power-button cold boot and a Cr50 `ecrst pulse` boot completed:

- Four CPUs up, zero CPU-init errors.
- Rust VBIOS POST and VBE 3.0 with 20 modes.
- Mode `0x1d4`: 1366x768, 32 bpp, 1408-pixel stride at `0xe0000000`.
- PSP DF lock, payload handoff and successful CrabEFI initialization.
- No emulator error, timeout, panic or host exception in either capture.

Logs and readbacks are retained locally under
`.local/rust-vbios/final-pr149-e2aa8a67-20261030/`. This does not prove OS boot,
visual panel output, persistent UEFI variables or transaction-by-transaction
parity. Earlier experiments did observe an AP-release #GP before VBIOS;
that failure did not occur in these two final-revision captures.

**Known blocker:** `--payload uefi-ui` does not link in the current 2 MiB reset
image. The linker reports `.data` overflowing the image by 2,528 bytes and
its load range overlapping `.init16` (`0x031fe000`). The reported `.data`
load end is `0x032009df`, so roughly 10.5 KiB must be recovered to clear that
reserved reset section, not merely the 2,528-byte region overflow. Do not
interpret the successful standard-UEFI boots as graphical-UI validation.

## Remaining scope

This is a VGA ROM driver, not yet a Rust Blobolator/FSP harness. The shared CPU
exits support such a consumer, but AMD passthrough policy, call-chain tracing
and transaction replay belong above the library. The VBIOS driver uses the
implemented virtual CPU feature set, not unsupported host CPUID features.
AVX/XSAVE and newer instructions are not supplied. New instruction support
belongs in rsemu's shared decoder/interpreter and its regression suite, not in
an AMD-only second emulator.
