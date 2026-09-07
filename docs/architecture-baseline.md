# Architecture migration baseline

<!-- markdownlint-disable MD013 -->

This tracks the initial increments of the [architecture migration](architecture.md).
The starting firmware source is revision `38586a3e`. Values below describe that
source, not proposed layout defaults or measured stack high-water marks.

## Initial scope

Implemented first: typed, versioned board discovery in
`tools/fbuild/src/board_manifest.rs`. All 14 live board manifests declare
`schema = 1`. Discovery reads the excluded board inventory directly, rejects
unknown fstart keys and malformed types, requires explicit identity, and validates
a single namespace for board and variant IDs before selection. It does not execute
board Rust. Schema 1 currently covers discovery fields only:

```toml
[package.metadata.fstart]
schema = 1
board = "qemu-riscv64"
platform = "riscv64"
target = "riscv64gc-unknown-none-elf"
stage-bin = "fstart-stage"
features = ["qemu-riscv64"]
```

Optional fields are `platform`, `target`, `stage-bin`, `features` (default empty),
`acpi-only-devices` (default false) and `variants` (default empty). Each variant
currently accepts only `features`, default empty. Board and variant IDs are
nonempty ASCII letters/digits/hyphens/underscores because they become artifact
path components. Other Cargo metadata namespaces remain unrestricted.

This preserves today's feature meanings and platform strings; it does **not**
pretend they are the target platform-profile/direct-dependency selection model.
Add layout/profile fields with the resolver that consumes them, rather than
accepting ignored configuration. There is no fallback to directory-derived board
identity or unversioned metadata.

## Representative source budgets

| Board/stage | Load placement | Stack | Heap | Source |
| --- | --- | --- | --- | --- |
| qemu-riscv64 monolithic | Flash `0x20000000`; writable data `0x81000000` | `0x100000` (1 MiB) | `0x40000` (256 KiB) | `crates/platform-qemu/src/virt.rs` |
| lenovo-x61 bootblock | Top-of-ROM sentinel `0xffffffff`, resolved by existing linker path | `0x2000` (8 KiB) | None | `crates/platform-intel/src/gm965.rs` |
| lenovo-x61 postcar | `0x01000000` | `0x2000` (8 KiB) | None | Same |
| lenovo-x61 ramstage | `0x04000000` | `0x400000` (4 MiB) | `0x200000` (2 MiB) | Same |

RISC-V virt flash is `0x02000000` bytes (32 MiB). These are current source budgets,
not independently maintained build inputs. The new resolver must preserve them
unless an explicit change is validated. The proposal's illustrative postcar
stack size is not the current X61 value.

## Fresh evidence

Before parser changes:

- `cargo run --locked -p fbuild -- build --board qemu-riscv64 --release --payload halt`
  succeeded. Firmware release compilation reported 1.52 seconds with an already
  populated cache; this is **not** a clean-build timing baseline.
- Produced ELF: 127,728 bytes; flat stage: 75,056 bytes. ELF size includes non-load
  content and is not the executable reservation budget.
- Existing warnings included unmatched selected-workspace profile overrides and
  an unused SiFive-U config field in the QEMU platform closure. This is evidence
  to inspect module gating, not proof that all SiFive code enters the final image.

After parser changes:

- `cargo test -p fbuild --lib`: 14 passed, including typed TOML handling, invalid
  metadata diagnostics, base/variant resolution, all identity collision shapes
  and discovery of the current board inventory.
- `QEMU_BOOT_TIMEOUT=45s bash ci/qemu-boot-tests.sh --board qemu-riscv64 --payload halt`:
  1 passed, 0 failed. The release image reached both `PCI root ready (` and
  `ramstage: ready for payload`. The halt case intentionally ended at timeout.
  Serial/build evidence is in `target/qemu-boot-tests/qemu-riscv64-halt.log`.
  This remains a development-integrity boot, not hardware-authenticated boot.

No fresh Intel release/hardware result, full board matrix, clean build timing,
stack high-water measurement or rust-analyzer workspace acceptance is claimed.
The previous X61 status remains cold-boot hardware validation outstanding.

## Layout wire and linker foundation

The next development increment implements the transport, not the board cutover:

- `crates/core/src/layout.rs`: allocation-free borrowed wire decoder, with a
  versioned 16-byte header and at most 32 physical range records. It rejects
  malformed/truncated/trailing bytes, nonzero reserved fields, unknown roles,
  duplicate singleton roles, empty ranges and overflowing ends. Image, writable,
  stack, heap and mapped-flash roles are singleton; additional reserved ranges
  may repeat. Overlap/containment policy belongs to the future resolver.
- `crates/image-build/src/layout.rs`: immutable host encoding using explicit
  little-endian fields, validated by the firmware decoder. Independent golden
  bytes check their agreement; no native struct cast is part of the ABI.
- `crates/stage/src/layout.rs`: one safe accessor encapsulates the unsafe linker
  boundary, bounds the length before forming a borrowed slice, and validates it.
  Migrated linkers must supply initialized, immutable stage-lifetime storage.
  Missing symbols are a link error, not a fallback to old board constants.
- `tools/fbuild/src/linker.rs`: descriptor `BYTE` emission for a caller-selected
  read-only load region. It is not called by the legacy layout generator.

The current linker puts heap/stack after actual linked sections. Those addresses
are not pre-link reservations. Do not feed them into this descriptor as if a
fixed-capacity `ResolvedBuild` already existed.

Validation:

- `cargo test --locked -p fstart-core -p fstart-image-build -p fbuild --lib`:
  34 tests passed.
- `cargo test --locked -p fstart-stage --lib layout::`: 1 test passed.
- `cargo run --locked -p fbuild -- build --board qemu-riscv64 --release --payload halt`:
  release link passed with the new API compiled for the firmware target. The
  descriptor accessor is not called by that board yet, so this is a build
  compatibility check, not proof of an integrated descriptor boot.
- The fbuild test invokes the pinned toolchain's real LLD for little-endian
  RISC-V and big-endian AArch64. With `--gc-sections`, it checks allocated,
  file-backed, non-writable descriptor bytes, 8-byte alignment, start/end
  symbols, and byte-for-byte preservation through the existing PT_LOAD flat
  extractor. Both outputs decode with the same little-endian wire reader.
  These are synthetic link fixtures, **not** boot or relocation evidence.

No production board consumes this API yet. This is staged development toward the
vertical slice, not another authoritative layout model or a completed migration.

## Next cutover

Finish QEMU RISC-V virt's build/runtime vertical slice: platform and board layout
metadata, host-only `ResolvedBuild` assigning fixed capacity/subranges, then wire
its linker projection, runtime accessor and assembler inputs together. Prove a
geometry-only change updates all consumers and forces relinking, validate actual
ELF ranges against reservations, and boot the migrated image. Remove that board's
host layout discovery path when the slice works. Preserve the existing
authenticated loading implementation.

In the same slice, prototype the platform-owned entry adapter, explicit cfg
selection, additive backend checks and bounded IDE/check view. Do not cut over
workspace/lock ownership until source identity, canonical dependency agreement,
proc macros, diagnostics and target/stage switching are demonstrated. Existing
board host tools, locks, runtime flash constants and feature forwarding remain
until their consuming scope is migrated; no parallel layout authority was added
by this discovery increment.
