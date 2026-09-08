# Selected firmware editor views

<!-- markdownlint-disable MD013 -->

`fbuild ide` generates an opt-in rust-analyzer view for a migrated board. It
supports QEMU RISC-V, ARMv7 and AArch64 monolithic flows and the Lenovo X61
Intel multistage flow. It does not modify the source
workspace, editor settings in the source tree, or committed/per-board locks.

## Use

```sh
cargo run --locked -p fbuild -- ide qemu-riscv64 --release --payload linux
cargo run --locked -p fbuild -- ide lenovo-x61 --release --payload uefi --stage ramstage
```

Rust-plan views select a named compiler unit from the platform's concrete plan;
multistage images require `--stage`. X61 currently exports `bootblock`, `postcar`,
`ramstage` and `smm`. The shared executor builds only that unit's producer closure
and records its actual artifacts in both editor and check environments. Thus
ramstage receives its explicitly bound SMM image, while bootblock/postcar do not.
AArch64's sole `stage` unit is selected by default. No family-specific selection
logic lives in IDE generation; see [the common-plan boundary](architecture-common-plan.md).

Open the printed `fstart.code-workspace` in VS Code. The generated files live in
`target/fstart-ide/<board>/<debug|release>/<payload>/`:

- `fstart.code-workspace`: repository folder plus selection-specific settings.
- `rust-analyzer.json`: the same flat `rust-analyzer.*` settings for other clients.
  Translate these into your client's nested initialization/settings format;
  `configuration()` in `ci/ide-tests.py` is a reference implementation. The LSP
  workspace/root URI must be the **source repository**, not this generated folder.
- `rust-project.json`: Cargo-derived crate graph with canonical source paths.
- `report.json`: selected workspace membership, root-lock pin comparison and any
  explicit all-board lock audit.

Use the project's pinned Rust toolchain with `rust-src` and a compatible
rust-analyzer. The live protocol probes used rust-analyzer 1.95.0-nightly
(`1ed4882`, 2026-02-25) and the pinned nightly-2026-02-26 toolchain's proc-macro
server. This is not an assertion that arbitrary older rust-analyzer versions
understand these configuration keys.

One active selection per editor session is supported. Generate another view and
open its workspace, or replace your client's settings with the newly generated
ones. RISC-V halt → Linux → CrabEFI → debug-halt → ARMv7 halt → Linux → AArch64
halt → Linux → CrabEFI → RISC-V halt switching passed in one LSP session without
restarting it, including original-source definitions, target/entry/pointer-width
cfgs and macro expansion after switches.

Ordinary Rust edits use live analysis and check-on-save. Regenerate the view after
changing manifests, features, layout, toolchain or build scripts. After modifying
proc-macro implementations/build-generated configuration, regenerate and restart
rust-analyzer to ensure its loaded macro libraries and configuration are fresh.
Automatic build-script/proc-macro hot-reload is not claimed. Failed producers
cannot silently become invented cfg/env/macro defaults.

## Why a Cargo-derived JSON graph

The first prototype linked the existing selected Cargo workspace directly.
Rust-analyzer loaded it, but a definition request from the **original** board file
returned nothing: the symlinked workspace paths did not establish usable source
identity. That failed prototype is not the shipped view.

The replacement takes:

1. The same resolved compiler invocation used by `fbuild build` and `check`.
2. Cargo's `-Zunstable-options --unit-graph` output (schema 1), including actual
   per-unit features, target/host distinction and renamed dependency edges.
3. A real Cargo check's build-script outputs and proc-macro artifact paths.

It projects those inputs into `rust-project.json`. Source roots are canonicalized,
not copied or generated. Host units do not inherit firmware selection cfgs.
Generated output directories belong to the corresponding source group, and
build-script cfg/env outputs apply to their consuming units, not to the producer
binary's compilation. Sysroot source and the matching proc-macro server are
available to rust-analyzer. The editor's sysroot source view is supplied by
rust-analyzer; Cargo remains responsible for the actual `-Zbuild-std` check.

`selection.rs` owns the target/profile/features/cfg flags for build, check and
editor checks. Encoded Rust flags preserve whitespace in paths and override
ambient Rust flags consistently. `rustflags.json` records their token boundaries; any `rustflags.txt` rendering
is diagnostic text, not shell-quoting instructions.

The check command emits Cargo JSON diagnostics. It runs once from the opened
repository so relative diagnostic filenames resolve to original source files.
The selected Cargo workspace is still used underneath for compilation, but is
**not** the language server's source graph. Compiler source errors may remain
while generating a view, provided required build-script/macro data is available.

## Build-lock prototype

```sh
cargo run --locked -p fbuild -- ide qemu-riscv64 --release --payload linux --audit-lock
```

This additionally creates `target/fstart-lock-prototype/`, using the original
workspace membership plus every discovered board. It seeds its lock from the
source root lock and lets Cargo resolve the excluded board packages. Neither this
manifest nor its lock is promoted into the source tree.

The audit compares the **same selected command** against both workspaces:

- External package version, source/revision and checksum pins must agree.
- Compiler-unit roots, canonical source identity, target, profile, features and
  dependency edges/aliases must agree after workspace-path/index normalization.
- The comparison includes build-script and standard-library compilation units;
  it is stronger than merely finding a package version somewhere in a lock.

A mismatch fails the explicit audit and leaves a report in the prototype folder.
The prototype currently contains 14 boards / 35 workspace members. The four
QEMU RISC-V, two ARMv7 and three AArch64 selections passed the comparison. This is a
selected-closure proof. X61 UEFI ramstage also passes this audit, and all three
Intel original-source graphs and per-role compiler selections were inspected.
These results are **not** a build or boot of every inventory member, nor a
claim of byte-identical binaries across differently located workspaces.

## Reproduce the acceptance checks

```sh
cargo test --locked -p fbuild --lib
python ci/ide-tests.py --audit-lock --edit-check --inventory-noise 100
```

`--edit-check` temporarily appends a deliberate type error to
`boards/qemu/riscv64/src/stage.rs`, checks compiler diagnostics on the original
file URI, and restores the source in a `finally` block. Do not run it with another
writer to that file. `--inventory-noise` temporarily creates owned test boards
under a fresh vendor directory and removes that directory afterward.

The probes verify:

- Definitions lead to original board/platform files, not workspace aliases.
- Live target/payload/profile/test cfgs and the selected launcher agree.
- Serde derives expand through the compiled proc-macro library.
- Compiler diagnostics appear on original source URIs and clear on restoration.
- An unrelated Intel platform symbol is not indexed.
- Adding 100 unrelated boards leaves the selected editor graph byte-for-byte
  equivalent after JSON parsing and does not break the same LSP operations.

One cached run took 2.56 seconds for initial halt startup plus semantic probes;
with 100 extra boards the same operations took 2.54 seconds. Switching Linux,
CrabEFI and debug-halt took 1.19, 2.23 and 1.54 seconds respectively. These are
single-run observations, **not** clean indexing/compile benchmarks. Protocol
messages and status/timing receipts live under `target/fstart-ide/proof/`.

## Remaining gates

Workspace/lock ownership is intentionally unchanged. Switching among all three
QEMU virt ISAs passed earlier probes. The concrete-plan correction additionally
passes live Intel CAR/postcar/RAM/SMM → AArch64 switching, macro expansion,
original-source fact navigation and invalid-fact diagnostics/recovery; current
receipts are linked from [common-plan acceptance](architecture-common-plan.md).
Clean-checkout regeneration,
broader inventory build coverage, and macro/build-script edit workflows need
further acceptance before removing existing workspace or board-lock paths.
