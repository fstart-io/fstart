# Architecture migration baseline

<!-- markdownlint-disable MD013 -->

This tracks the first increment of the [architecture migration](architecture.md).
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

## Next cutover

The next scope is QEMU RISC-V virt's build/runtime vertical slice: platform and
board layout metadata, host-only `ResolvedBuild`, linker projection, core borrowed
layout view, stage accessor and assembler consumption. Remove that board's host
layout discovery path when the slice works. Preserve the existing authenticated
loading implementation.

In the same slice, prototype the platform-owned entry adapter, explicit cfg
selection, additive backend checks and bounded IDE/check view. Do not cut over
workspace/lock ownership until source identity, canonical dependency agreement,
proc macros, diagnostics and target/stage switching are demonstrated. Existing
board host tools, locks, runtime flash constants and feature forwarding remain
until their consuming scope is migrated; no parallel layout authority was added
by this discovery increment.
