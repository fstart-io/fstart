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

On 2026-10-30 the Rust-only revision
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
that failure did not occur in those two captures.

### Larger Picasso reset image

The previous `uefi-ui` link failure was caused by the framework's 2 MiB
reservation, not a hardware limit. Picasso now reserves 4 MiB for the PSP
reset image at `0x03000000..0x03400000`, with `.init16` at `0x033fe000` and
the reset vector at `0x033ffff0`. The early e820 reservation extends through
`0x03400000`. The flash reset window moves from `0x980000` to `0x780000`,
derived from the image size and fixed FFS start; neither the FFS nor PSP
partition moves, and the flash chip remains 16 MiB.

Both `uefi` and `uefi-ui` assemble successfully with this layout. The UI's
code/data ends at `0x03200a60`, leaving 2,086,304 bytes before `.init16`.
All 21 platform and 65 image-builder tests pass, including both 2 MiB and
4 MiB packed-image round trips and a UI plan/linker/reservation regression.
The platform's strict Clippy check with `--no-deps` passes; an unfiltered
strict check encounters existing warnings in unchanged dependencies.

Revision `e1ea98dcc47c1fff5dcc70ae7fea94b8d17ac3b2` was then tested with
`uefi-ui` on Vilboz: both a G3/power-button boot and a Cr50 `ecrst pulse`
boot passed the four-CPU, Rust POST, VBE and CrabEFI initialization checks.
Both captures confirm e820 reserves `0x02000000..0x03400000` and the FFS is
still mounted at `0xffb80000`. Neither shows an emulator failure, host
exception, panic or allocator abort.

Two fresh full recovery reads matched before writing, and an independent
backup was retained. The programmed UI image matched a full readback before
boot; its SHA-256 is
`dead01d2aac7ba58f1e73a0233d7a6b5861dc7d9b753688b376d4449e3c6506f`.
Evidence is under `.local/rust-vbios/ui-4m-20261030/`. These captures stop
shortly after CrabEFI initialization; visible menu output and OS boot were
not verified.

## Remaining scope

This is a VGA ROM driver, not yet a Rust Blobolator/FSP harness. The shared CPU
exits support such a consumer, but AMD passthrough policy, call-chain tracing
and transaction replay belong above the library. The VBIOS driver uses the
implemented virtual CPU feature set, not unsupported host CPUID features.
AVX/XSAVE and newer instructions are not supplied. New instruction support
belongs in rsemu's shared decoder/interpreter and its regression suite, not in
an AMD-only second emulator.
