# Editor support for board crates

Board crates live in `boards/<vendor>/<board>/` and are excluded from the root
workspace (`exclude = ["boards"]` in `Cargo.toml`). `cargo fbuild ide` generates
a rust-analyzer configuration for one board and one firmware selection. It writes
only under `target/`; no tracked file, editor setting or `Cargo.lock` is changed.

## Generate a view

```sh
cargo fbuild ide qemu-riscv64 --release --payload linux
cargo fbuild ide lenovo-x61 --release --payload uefi --stage ramstage
```

`cargo fbuild` is the alias `run --package fbuild --` from `.cargo/config.toml`.
The command prints the path of the generated `fstart.code-workspace`.

| Argument / flag | Meaning |
| --- | --- |
| `<board>` | Board name from `[package.metadata.fstart] board`, e.g. `qemu-riscv64`, `lenovo-x61`, `foxconn-d41s`. |
| `--payload <p>` | `uefi`, `uefi-ui`, `uefi-basic`, `linux`, `coreboot`, `halt`. |
| `--release` | Release profile. Default is debug. |
| `--stage <unit>` | Compilation unit from the platform plan. Required when the plan has several units; the error lists the names. Single-unit boards (QEMU) select their only unit. |
| `--audit-lock` | Also run the all-board lock audit (see below). |
| build options | The same board options as `cargo fbuild build` (e.g. `--image-config`, `--blobs`, `--donor`, `--compiler-input`, `--payload-file`) are accepted. |

Generation runs `cargo metadata`, a unit-graph query and a real `cargo check`
for the selected build (see [How the graph is built](#how-the-graph-is-built)).
Source compile errors do not stop generation; a failed build-script or
proc-macro producer does.

## Generated files

Output directory:

```text
target/fstart-ide/<board>/<debug|release>/<payload>/[<unit>/]
```

`<unit>/` is added unless the unit is named `stage` (single-unit platforms).

| File | Contents |
| --- | --- |
| `fstart.code-workspace` | Folder = repository root, plus the `settings` below. |
| `rust-analyzer.json` | The same `rust-analyzer.*` settings as flat dotted keys. |
| `rust-project.json` | Crate graph (`linkedProjects`) with canonical source paths. |
| `report.json` | Board, target, payload, unit, workspace members, comparison of external `Cargo.lock` pins against the root `Cargo.lock`, and `all_board_lock_prototype` when `--audit-lock` is given. `workspace_ownership_cutover` is always `false`. |

Settings in `rust-analyzer.json` / the workspace file:

| Key | Value |
| --- | --- |
| `linkedProjects` | `[<output dir>/rust-project.json]` |
| `cargo.target` | Board target triple. |
| `cargo.cfgs` | `[]` |
| `cfg.setTest` | `false` |
| `cargo.extraEnv`, `check.extraEnv` | Selection environment: `CARGO_ENCODED_RUSTFLAGS` and an empty `RUSTFLAGS`. |
| `check.overrideCommand` | `cargo check --message-format=json` with the selection arguments (prefixed by `env -u` for removed variables). |
| `check.invocationStrategy` | `once` |
| `procMacro.enable` | `true` |

Supporting files from the build, under
`target/fstart-build/<board>/<debug|release>/<sha256>/`:

- `resolved-build.json`, `compiler-selection.json`
- `rustflags.json` (`rustflags` and `member_rustflags` token lists)
- `member-rustflags.toml` (cfgs and linker script, for workspace members only)
- `ide-unit-graph.json`, `ide-cargo-messages.jsonl`

The selected manifest is `target/fstart-workspaces/<board>/Cargo.toml`. All
builds share the Cargo target directory `target/fstart-build/cargo`.

## Point an editor at it

- **VS Code:** open the printed `fstart.code-workspace`.
- **Other clients:** load `rust-analyzer.json` and convert the flat keys to the
  nested form your client expects. The workspace root must be the repository
  root, not the generated directory.
- **Toolchain:** `rust-toolchain.toml` pins `nightly-2026-10-08` with `rust-src`.
  `rust-analyzer` must be a real binary on `PATH`. A rustup proxy works only if
  the toolchain's `rust-analyzer` component is installed.
- **Switching views:** load the next view's `rust-analyzer.json` and send
  `workspace/didChangeConfiguration`; the server session can stay up.
- **Regenerate** after changing manifests, features, build scripts, the toolchain
  or the platform plan. After changing a proc-macro implementation, regenerate
  and restart rust-analyzer.
- `.dir-locals.el` (eglot `:cargo :features ["acpi"]` for `rustic-mode`) is not
  used by `fbuild ide`.

## How the graph is built

1. Cargo's unit graph (`cargo check <selection args> -Zunstable-options --unit-graph`,
   version 1 required) gives the units, features, targets and dependency edges.
2. A real `cargo check --message-format=json` supplies build-script outputs
   (cfgs, `OUT_DIR`, env) and proc-macro dylib paths.
3. The projection in `tools/fbuild/src/ide_graph.rs`:
   - `std` units and build-script run units are skipped.
   - Each unit gets `feature=` cfgs, `debug_assertions` for debug profiles, and
     the build-script cfgs and env of its own package.
   - `OUT_DIR` is added to the package's source include directories.
   - `sysroot` is `rustc --print sysroot`; `sysroot_src` is
     `<sysroot>/lib/rustlib/src/rust/library`.
4. Missing build-script output or a missing proc-macro artifact is an error.
   Nothing is guessed.

## Lock audit (`--audit-lock`)

`tools/fbuild/src/ide_lock.rs` and `build_board::prepare_inventory_workspace`:

- Creates `target/fstart-lock-prototype/`: symlinks to `crates/`, `tools/`,
  `boards/`, a copy of the root `Cargo.lock`, and a `Cargo.toml` whose members
  add every board from `boards/*/*/Cargo.toml`.
- Runs the same selection's unit-graph query against that manifest and compares
  it with `ide-unit-graph.json`. Source paths are canonicalized and package IDs
  and dependency indices are normalized before comparison.
- Checks that every external pin (name, version, source, checksum) in the
  selected `Cargo.lock` is in the prototype lock.
- Writes `report.json` in the prototype directory. On mismatch the command fails.
  Nothing is promoted into the source tree.

## Tests

| Command | Requires | Covers |
| --- | --- | --- |
| `cargo test --locked -p fbuild --lib` | toolchain | Unit tests for `ide.rs` (pin comparison), `ide_graph_tests.rs`, `ide_lock.rs` (graph normalization). |
