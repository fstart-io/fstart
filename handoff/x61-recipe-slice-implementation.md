Implemented the Lenovo X61 static typed BSP -> GM965/ICH8 recipe -> generic selected-board wrapper slice.

Changed files:

- `Cargo.toml`
- `Cargo.lock`
- `boards/lenovo-x61/Cargo.toml`
- `boards/lenovo-x61/src/lib.rs`
- `boards/lenovo-x61/src/mainboard.rs`
- `boards/lenovo-x61/src/smm.rs`
- `boards/lenovo-x61/src/stage.rs`
- `crates/fstart-platform-intel-gm965-ich8/Cargo.toml`
- `crates/fstart-platform-intel-gm965-ich8/src/lib.rs`
- `crates/fstart-platform-intel-gm965-ich8/src/recipe.rs`
- `crates/fstart-stage/Cargo.toml`
- `crates/fstart-stage/src/fixed_helpers.rs`
- `crates/fstart-stage/src/lib.rs`
- `crates/fstart-stage-runtime/src/fixed_flow.rs`
- `crates/fstart-stage-runtime/src/lib.rs`
- `xtask/src/build_board.rs`
- `xtask/src/build_plan.rs`
- `handoff/x61-recipe-slice-implementation.md`

What changed:

- X61 `Board` now implements `FirmwareBoard` with `type Recipe = Gm965Ich8UefiRecipe<Self>`.
- X61 stage binding is trait/binding code only; no board-owned entrypoint, bootblock adapter, or ramstage adapter.
- X61 mainboard/dock/DLPC/ACPI/SMBIOS/SMM facts stay in `boards/lenovo-x61`.
- GM965/ICH8 owns reusable bootblock/ramstage recipe sequencing and platform hooks.
- `xtask` uses one generic FirmwareBoard/StageRecipe selected-board wrapper; no GM965-specific X61 wrapper path remains.
- Removed broad-diff drift from unrelated board/platform migrations and removed backup dirs from the working tree.
- Pinned `sha2` to `force-soft` because the target `x86_64-unknown-none` build hit a rustc/LLVM crash in the default `sha2` backend.

Architecture evidence:

- Generated wrapper dependency aliases the selected BSP as `fstart-board-selected = { package = "fstart-board-lenovo-x61", path = "/home/arthur/src/fstart/boards/lenovo-x61", default-features = false, features = ["stage"] }`.
- Generated wrapper calls `fstart_stage::run_board::<Board>(fstart_stage::StageKind::from_option(option_env!("FSTART_STAGE_NAME")), handoff_ptr)`.
- `boards/lenovo-x61/src/stage.rs` contains the `FirmwareBoard` and `Gm965Ich8UefiBoard` impls and no entrypoint.
- `xtask/src/build_board.rs` has no `gm965-ich8-uefi` special-case wrapper branch.
- `find boards crates -maxdepth 1 -type d -name '*backup*' -o -type d -name 'fstart-mainboard-lenovo-x61'` produced no in-tree matches.

Remaining non-X61 legacy paths intentionally untouched:

- Existing Foxconn/QEMU/Sunxi/Sifive board stage paths were restored/left as legacy, not migrated in this slice.
- Existing bespoke `board-stage-wrapper`, `sunxi-mmc-linux`, and `qemu-virt-linux` wrapper paths remain for their legacy boards.

Validation:

- `cargo xtask build --board lenovo-x61` passed after the `sha2` `force-soft` fix; built `fstart-bootblock.bin` and `fstart-ramstage.bin` for debug.
- `cargo xtask build --board lenovo-x61 --release` passed; built release `fstart-bootblock.bin` and `fstart-ramstage.bin`.
- `cargo test -p xtask build_board` passed: 1 test passed.
- `cargo test -p xtask build_plan` passed: 4 tests passed.
- `cargo fmt --check` passed.
- `git diff --cached --name-only` produced no output.

Residual risks:

- X61 board-owned SMM handler is preserved, but SMM remains not wired into the selected-board firmware build in this slice (`Gm965Ich8Config` still emits `smm: None`).

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Only the X61 BSP, GM965/ICH8 recipe, generic stage wrapper/runtime, xtask planning, and sha2 build workaround changed; unrelated board/platform migrations were restored and backup dirs removed."
    },
    {
      "id": "criterion-2",
      "status": "satisfied",
      "evidence": "Report includes changed files, tests, commands, validation summaries, residual risks, no-staged-files evidence, and generated-wrapper architecture evidence."
    }
  ],
  "changedFiles": [
    "Cargo.toml",
    "Cargo.lock",
    "boards/lenovo-x61/Cargo.toml",
    "boards/lenovo-x61/src/lib.rs",
    "boards/lenovo-x61/src/mainboard.rs",
    "boards/lenovo-x61/src/smm.rs",
    "boards/lenovo-x61/src/stage.rs",
    "crates/fstart-platform-intel-gm965-ich8/Cargo.toml",
    "crates/fstart-platform-intel-gm965-ich8/src/lib.rs",
    "crates/fstart-platform-intel-gm965-ich8/src/recipe.rs",
    "crates/fstart-stage/Cargo.toml",
    "crates/fstart-stage/src/fixed_helpers.rs",
    "crates/fstart-stage/src/lib.rs",
    "crates/fstart-stage-runtime/src/fixed_flow.rs",
    "crates/fstart-stage-runtime/src/lib.rs",
    "xtask/src/build_board.rs",
    "xtask/src/build_plan.rs",
    "handoff/x61-recipe-slice-implementation.md"
  ],
  "testsAddedOrUpdated": [
    "xtask/src/build_board.rs: build_board_firmware_wrapper_stage_feature_allowlist_excludes_recipe_drivers",
    "xtask/src/build_plan.rs: plan_selects_security_flow_for_sigverify"
  ],
  "commandsRun": [
    {
      "command": "jj status && jj diff --stat",
      "result": "passed",
      "summary": "Initial broad working-copy drift inspected; later status reduced to X61/generic wrapper slice."
    },
    {
      "command": "cargo xtask build --board lenovo-x61",
      "result": "failed",
      "summary": "First attempt failed compiling sha2 for x86_64-unknown-none with rustc-LLVM ERROR before applying force-soft."
    },
    {
      "command": "cargo xtask build --board lenovo-x61",
      "result": "passed",
      "summary": "Debug bootblock and ramstage built successfully after sha2 force-soft."
    },
    {
      "command": "cargo xtask build --board lenovo-x61 --release",
      "result": "passed",
      "summary": "Release bootblock and ramstage built successfully."
    },
    {
      "command": "cargo test -p xtask build_board",
      "result": "passed",
      "summary": "1 test passed; generic wrapper feature allowlist covered."
    },
    {
      "command": "cargo test -p xtask build_plan",
      "result": "passed",
      "summary": "4 tests passed; security-flow planning covered."
    },
    {
      "command": "cargo fmt --check",
      "result": "passed",
      "summary": "Formatting check passed."
    },
    {
      "command": "git diff --cached --name-only",
      "result": "passed",
      "summary": "No staged files."
    },
    {
      "command": "jj status",
      "result": "passed",
      "summary": "Working copy contains the reported modified files plus this handoff report."
    }
  ],
  "validationOutput": [
    "debug build: built target/x86_64-unknown-none/debug/fstart-bootblock.bin and fstart-ramstage.bin",
    "release build: built target/x86_64-unknown-none/release/fstart-bootblock.bin and fstart-ramstage.bin",
    "generated wrapper aliases fstart-board-lenovo-x61 as fstart-board-selected",
    "generated wrapper dispatches through fstart_stage::run_board::<Board>(StageKind::from_option(option_env!(\"FSTART_STAGE_NAME\")), handoff_ptr)",
    "xtask grep for literal gm965-ich8-uefi in build_board.rs returned no matches",
    "backup/mainboard-dir find check returned no in-tree matches"
  ],
  "residualRisks": [
    "X61 SMM handler is preserved in the board crate but not wired into the selected-board firmware build in this slice."
  ],
  "noStagedFiles": true,
  "diffSummary": "Narrowed broad migration to X61 BSP bindings, GM965/ICH8 reusable recipe, generic selected-board wrapper generation, fixed-flow recipe trait plumbing, and a sha2 force-soft target build workaround.",
  "reviewFindings": [
    "implementation self-check found no blockers; required reviewer gate still pending"
  ],
  "manualNotes": "Do not run lens_diagnostics; it was not run. Unrelated board migrations were restored/left untouched."
}
```
