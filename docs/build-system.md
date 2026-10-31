# Board builds and image assembly

`cargo fbuild` is a Rust orchestrator over Cargo. It does not replace Cargo's
compiler cache, and board crates do not author arbitrary build workflows.

## Responsibility boundaries

- `boards/<vendor>/<board>/Cargo.toml` declares discovery identity, variants,
  a stage binary and a direct platform dependency (`build-profile`). Board
  Rust supplies typed hardware facts and board wiring.
- `crates/platform-*/src/host*` turns those facts and an explicit `BuildSelection`
  into a `BuildPlan`. The platform owns stage budgets, payload capabilities,
  feature bundles, compiler selections and image input capacities.
- `tools/fbuild` discovers boards, parses the command once, executes the plan
  with Cargo and publishes its current artifacts. Build/check/editor views use
  the same compiler selection and producer closure.
- `crates/image-build` owns reusable ELF/linker helpers, FFS packaging, signing
  and image-format mechanics. Assembly consumes `FfsAssembly` directly, not
  a synthetic runtime `BoardConfig`. AMD's compositor combines that FFS with
  reset stages and packed/vendor PSP firmware.

The physical HiFive Unmatched board uses `platform-sifive`, not a QEMU machine
preset. Its live FU740 configuration also supplies its host image facts; flash
and payload addresses are not independently repeated in QEMU policy.

## Resolution and compilation

Boards are outside the root Cargo workspace. fbuild generates a selected
workspace under `target/fstart-workspaces/<board>/`, seeded with the root lock.
It builds a tiny host adapter that invokes `platform::Plan::<board::Board>::emit`.
The returned JSON is the concrete execution contract, not a second board schema.
Consequently `explain` may compile a host adapter too.

A plan's compilation units declare target, features, compiler cfg vocabulary,
linker script, bounded file inputs and producer-artifact bindings. The executor
validates the complete graph before compilation. An SMM image, for example,
is an explicit producer input to the firmware unit that embeds it. Compiler
files are snapshotted and content-bound; output ELF geometry is validated.

Commands have distinct results:

```sh
cargo fbuild explain qemu-riscv64 --payload halt
cargo fbuild build -b qemu-riscv64 --release --payload halt
cargo fbuild build -b tyan-s8030 --release --stage smm
cargo fbuild assemble -b qemu-riscv64 --release --payload halt
cargo fbuild check qemu-riscv64 --payload halt
```

`build` produces unit artifacts. `assemble` builds them and produces the image.
`run` selects the existing emulator launch path and assembles boot media when
required. Image-format differences (eGON headers, Intel descriptors, AMD PSP
banks) belong to the image-format implementation, not board-name dispatch.

## Assembly inputs

`BuildPlan.inputs` is the platform's complete external payload-input inventory:
`kernel`, `firmware`, `fit`, and `dtb`. Each entry declares its default path and
capacity. Plan validation rejects payload configurations that require
undeclared inputs.

`--kernel`, `--firmware`, and `--fit` are aliases for named inputs; use
`--input dtb=board.dtb` for an override. Unknown or duplicate inputs, absent
required files, and empty/oversized files fail. Defaults and supplied files
pass through the same resolver and capacity checks.

## Cache reuse and workspace ownership

Every board and firmware unit uses **one** Cargo target directory:

```
target/fstart-build/cargo/
```

Host adapters likewise share `target/fstart-host/cargo/`. Cargo decides which
outputs are compatible by its own fingerprints. Workspace isolation must not
turn into a per-board compiler cache: identical target libraries, registry
dependencies and build tools remain reusable across boards. Linker scripts are
passed only to the final binary using `cargo rustc`, never to its dependencies.

Libraries opt in to just the cfg axes they read using
`package.metadata.fstart.compiler-cfg` in their own Cargo manifest. For example:

```toml
[package.metadata.fstart.compiler-cfg]
fstart_stage_env = ["monolithic", "car", "postcar", "ram", "smm"]
```

These per-package profile flags use the library's stable source-check vocabulary,
not the vocabulary of whichever board selected it. Libraries such as core, log,
crypto and FFS receive no selection-specific profile flags. The board itself
still receives all three platform-validated selectors. Cargo can therefore
reuse compatible first-party library outputs as well as std/registry artifacts.
`cache_reuse_tests` compiles two distinct board packages with different linker
scripts and verifies identical, `fresh=true` library artifacts, including a cfg
consumer, rather than merely comparing target-directory names. Changing a consumed
axis must rebuild that consumer while leaving disjoint libraries fresh. A source
syntax audit requires declarations for first-party cfg consumers; target cfg
consumers used as host libraries/proc macros are rejected from Cargo's actual unit
graph. Build scripts must not read target selectors.

Generated manifests and lock augmentation use structured TOML operations,
not source formatting markers. The root manifest/lock and board source files
are not rewritten. A checkout-wide OS advisory lock at
`target/fstart-orchestration.lock` is acquired before profile discovery and held
through compilation and image publication. This covers mutations and current
artifact copies that Cargo's own compilation lock does not protect. Different
fbuild processes in one checkout serialize; separate checkouts remain independent.
Nested operations on the same thread share the guard instead of self-deadlocking;
other threads/processes still serialize. Generated files are atomically published,
and identical contents are left untouched. The augmented lock is published before
the manifest that adds its runner. Editor checks use `fbuild editor-check` with the
recorded selection and acquire the same guard; regenerate old IDE workspaces to
adopt this command. Manually running Cargo on generated files bypasses that guard.
The OS releases ownership on exit, including after a crash.

QEMU and probe-rs receive session-private copies of the exact published image/ELF
pair. The build lock is released before either tool starts waiting interactively,
so another build or editor check can run without overwriting their launch media.
Sessions hold their own OS file lock. A subsequent launch removes abandoned
sessions left by terminated processes, without deleting active sessions. Normal
exit removes the session and its temporary pflash/SD files.
Custom probe-rs binaries are selected explicitly with `flash --probe-rs PATH`
instead of a guessed `~/src/probe-rs` location.

Resolved-plan and compiler-input hashes identify artifact directories and
receipts. They are not a claim that the entire build is hermetic or a remotely
reusable content-addressed build: source/toolchain tracking remains Cargo's job.
For repeatable assembly supply an explicit signing key and SMBIOS date (or
`SOURCE_DATE_EPOCH`); defaults use an ephemeral key and the current date.
