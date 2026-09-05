# Authenticated boot implementation validation

These are implementation measurements, not evidence of hardware secure boot.
Release is the reference configuration. Commands use the repository's pinned
Rust toolchain and the boards' default build configuration unless noted.

## Size baseline

The baseline is Jujutsu change `qorunkxy`, commit `9f4bee99`, preserved before
implementation and built in a separate workspace. It already includes the
byte-emitting ACPI backend, borrowed anchor and compact SHA-2 changes. Comparing
against the old HTML plan's 467 KiB ramstage would misattribute earlier work.

Final integrated Lenovo X61 release measurement (bytes):

| Stage | Before flat binary | After flat binary | Before text + rodata (`size` text column) | After text + rodata |
| --- | ---: | ---: | ---: | ---: |
| bootblock | 131,072 | 126,976 | 121,604 | 114,924 |
| postcar | 12,336 | 16,576 | 11,996 | 16,221 |
| ramstage | 176,000 | 150,728 | 175,646 | 150,365 |

Section detail, before → after (bytes):

| Stage | `.text` | `.rodata` | `size` data total | `size` BSS total |
| --- | ---: | ---: | ---: | ---: |
| bootblock | 75,234 → 70,098 | 46,232 → 44,688 | 328 → 344 | 12,304 → 12,304 |
| postcar | 10,180 → 14,061 | 1,768 → 2,112 | 328 → 344 | 12,296 → 12,296 |
| ramstage | 131,954 → 113,725 | 43,644 → 36,592 | 336 → 352 | 6,565,888 → 6,565,920 |

Named `.text`/`.rodata` exclude auxiliary sections such as reset code. GNU
`size` totals include custom sections; BSS totals include reserved heap/stack,
not measured peak usage.

Postcar intentionally grows to authenticate ramstage before entry. Symbol
inspection finds the public-key/curve implementation in bootblock, not in
postcar or ramstage. Flat bootblock size also reflects reset-vector placement
and alignment; it is not simply the sum of function sizes.

The integrated X61 FFS image is 198,450 bytes inside the 4 MiB flash layout.
A comparable baseline flash number is unavailable: assembling the preserved
baseline fails with `compressed FSTART_ANCHOR patching did not converge`.
Its stage link/ELF measurements above succeed. Do not report a measured flash
saving against that failed baseline assembly.

## Completed checks

- Release links exercised all 14 declared board IDs: Lenovo X61, Intel
  D945GCLF, Foxconn D41S, six QEMU boards, SiFive Unmatched, and four Sunxi
  boards. The QEMU set is q35, aarch64, armv7, riscv64, sbsa and sifive-u.
- X61 image assembly and root/directory inspection succeed.
- QEMU Q35 authenticates its image and reaches payload readiness. Its pflash
  packager now copies the complete shared anchor size, including image family.
- Banana Pi M1, Orange Pi PC2 and Lichee RV Dock assemble with `--payload halt`.
  Independent SHA-256 checks confirm the emitted directory and stored
  bootstrap hashes and the initial SRAM image's exact mainstage pin.
- QEMU RISC-V with `--release --payload halt` authenticates boot media and
  reaches `ready for payload`. The test terminates the deliberate halt loop
  with a timeout; timeout is not interpreted as successful boot by itself.
- Flipping a directory byte in that pflash image produces `boot_integrity
  failed` and never reaches payload readiness.
- A RISC-V image containing synthetic firmware and kernel stubs completes
  bounded FDT preparation and prints markers from both verified entries.
  Corrupting its compressed kernel bytes leaves directory authentication
  successful but produces `FFS payload load failed`, with neither entry marker.
  These stubs exercise handoff; they are not an OpenSBI or Linux boot test.
- The complete X61 DSDT passes its host ACPICA round-trip test.
- ACPI/macro and Intel/Lenovo/Super I/O driver tests pass in debug and release,
  including operand overflow, failed assembly capacity, and simulated
  DSDT+SSDT method behavior (130 passed, 13 ignored in each profile).
- Root/directory integration tests pass in debug and release. Image builder,
  crypto and fbuild host tests also pass.
- All 14 stage tests pass with Linux, FIT, FDT, signatures and LZ4 enabled in
  debug and release. They include configured-entry mismatch, retained load
  overlap/capacity/policy refresh, FDT source/capacity and cumulative-growth
  failures. The feature matrix also checks hash-only bootstrap and
  signature-free mainstage.

Useful reproduction commands:

```sh
cargo fbuild build --board lenovo-x61 --release
cargo fbuild assemble --board lenovo-x61 --release
cargo fbuild assemble --board bananapi-m1 --release --payload halt
cargo fbuild inspect --image target/ffs/bananapi-m1.ffs
cargo fbuild run --board qemu-riscv64 --release --payload halt
cargo test -p fstart-ffs --features std
cargo test -p fstart-stage --features ffs,ffs-signature,lz4,fit
cargo test -p fstart-acpi -p fstart-acpi-macros --all-features
```

## D945GCLF physical boot (2026-09-05)

A release image assembled with `cargo fbuild assemble --board intel-d945gclf
--release` was flashed to the board's 512 KiB Winbond W25X40 using Dedipico /
`rflasher`. The original chip contents were backed up and verified before
writing; the new image also passed flash verification. UART capture was drained
before the power-on pulse and recorded for 60 seconds at 115200 baud.

The first boot stopped after `jumping to postcar at 0x1000000`. Comparison with
coreboot identified two pre-existing defects:

- i945 egress/DMI VC configuration passed a keep-mask as a clear-mask, erasing
  the VC1 ID and leaving a mismatch with ICH7. Clearing only the TC1–TC7 field
  preserves the ID, enable bits, and TC0 mapping.
- CAR teardown returned through the inherited CAR stack after disabling CAR.
  It now captures the return address in R11 before teardown and jumps through
  that register, following coreboot's non-evict teardown approach.

With both corrections, the DMI negotiation timeout disappeared. The opt-in
`intel-d945gclf-postcar-debug` fbuild variant emits raw COM1 checkpoints;
`ptmcr` confirmed postcar entry, return from teardown, MTRR enable, completion
of INVD, and entry into Rust. Default production images omit those writes. Postcar verified and entered ramstage;
mainstage authenticated boot media, emitted ACPI/SMBIOS tables, and reached
`intel-ich7: finalize complete`. This was a no-payload boot, not an OS boot.
Because both corrections were tested together, this does not isolate the
stall's cause to one instruction. The log still reports absent-slot SMBus
transaction errors and `microcode: no matching Intel patch found` warnings.

Local evidence is under `/tmp/d945gclf-car-vc-fix-20260905/` (`firmware.pflash`,
`boot.log`, `image-inspect.txt`). The flashed image SHA-256 is
`25b22b5130add6ea8f9ca86ca6f7081b60a3a6b53b66c85dcaff47af54c30f4d`.
The original backup is `/tmp/d945gclf-authboot-20260905/before.bin`.
Power-off was commanded after capture. Formatting and the Intel driver release
tests passed (23 unit tests and 3 doctests; 4 doctests ignored).

## Remaining hardware validation

The D945GCLF test above covers one physical Intel CAR-transition boot through
mainstage finalization. Other Intel boards, Sunxi boot, suspend/resume,
power-fail update, DMA isolation, and persistent rollback remain untested.
Link-time SRAM fit is not a measured peak call-stack bound. Further hardware
validation remains required before broader support or authenticated-update
protection claims. Existing products remain labeled development-integrity.
