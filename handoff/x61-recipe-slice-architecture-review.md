# X61 recipe-slice architecture review

## 1. Verdict

The current reduced diff now mostly moves toward `docs/board-support-package-platform-recipe-plan.md` and `docs/rust-board-builder-stage-flow-plan.md` for the Lenovo X61 path: board facts are in `boards/lenovo-x61`, the GM965/ICH8 recipe owns sequencing, and `xtask` no longer has a GM965-specific selected-stage wrapper branch. I would not call for a whole-repo fresh start.

However, the diff is not merge-clean as-is: the shared `StaticBoard` -> `StageFlow` rename breaks existing legacy board stage builds while the worker claimed those paths were merely restored/left untouched. Treat the X61 slice as salvageable, but fix that regression before piling on SMM/ACPI work.

## 2. Findings

### Blocker

- **Unrelated legacy board stage builds are broken by the shared API rename.** The diff removes `fstart_stage_runtime::StaticBoard` and `fstart_stage::run_static_board` in favor of `StageFlow`/`run_stage_flow` (`crates/fstart-stage-runtime/src/lib.rs:11`, `crates/fstart-stage-runtime/src/fixed_flow.rs:12`, `crates/fstart-stage/src/lib.rs:94-101`), but existing board-local stage adapters still import/call the removed names (`boards/foxconn-d41s/src/stage/bootblock.rs:9`, `boards/foxconn-d41s/src/stage/mod.rs:39`). Verified with `cargo check -p fstart-board-foxconn-d41s --features stage --target x86_64-unknown-none`: it fails with `no StaticBoard in the root` and `run_static_board not found`. This is hidden broad-diff fallout, not an X61-only slice.

### High

- **SMM still does not match the BSP/selected-board ownership model.** X61 has a board-owned handler (`boards/lenovo-x61/src/smm.rs:12-29`), but the GM965/X61 path disables SMM in board config and MP init (`crates/fstart-platform-intel-gm965-ich8/src/lib.rs:275`, `crates/fstart-platform-intel-gm965-ich8/src/lib.rs:512-515`, `boards/lenovo-x61/src/stage.rs:67-70`). The current SMM stage also uses a central `smm_platform = "lenovo-x61"` branch with `NoBoardSmmHandler` (`crates/fstart-smm-stage/src/lib.rs:13-14`, `crates/fstart-smm-stage/src/lib.rs:80-82`) and host-side string matching (`crates/fstart-smm-image/src/main.rs:64-69`). That is exactly the remaining mismatch the worker reported; do not count SMM as aligned yet.

### Medium

- **ACPI path plumbing is only halfway to the doc.** Board ACPI moved into `boards/lenovo-x61` and takes a platform-provided path object (`boards/lenovo-x61/src/mainboard.rs:398-401`, `boards/lenovo-x61/src/mainboard.rs:429-430`), which is progress. But `Gm965Ich8AcpiPaths` still returns hard-coded absolute namespace strings such as `"\\_SB_.PCI0.LPCB"` from the platform crate (`crates/fstart-platform-intel-gm965-ich8/src/lib.rs:52-115`) rather than an `AcpiContext` derived from topology as required by `docs/board-support-package-platform-recipe-plan.md:358-377`.

- **The new “generic” FirmwareBoard wrapper is recipe-generic, not platform-generic.** `xtask` no longer has a `gm965-ich8-uefi` special branch (`xtask/src/build_board.rs:390-399`), and the generated wrapper calls `fstart_stage::run_board::<Board>` (`target/fstart-build/lenovo-x61/selected-stage/src/main.rs:37-43`). But `selected_firmware_board_cargo_toml` and generated `main.rs` still hard-code `fstart-platform-x86_64` (`xtask/src/build_board.rs:955-959`, `xtask/src/build_board.rs:1017`). Fine for X61, but do not claim this is a fully generic selected-board wrapper for non-x86 recipes yet.

### Low

- **Generic/common code still carries a `Southbridge` concept.** The GM965 recipe imports `fstart_services::Southbridge` (`crates/fstart-platform-intel-gm965-ich8/src/recipe.rs:11-13`), and X61 dock detection takes `&dyn fstart_services::Southbridge` (`boards/lenovo-x61/src/mainboard.rs:178-183`). This appears pre-existing, but it remains a doc mismatch against the “generic framework code should not learn concepts like southbridge” rule (`docs/board-support-package-platform-recipe-plan.md:50-51`, `docs/rust-board-builder-stage-flow-plan.md:38-39`).

## 3. Alignment checklist

- **X61 static typed BSP:** PASS. `boards/lenovo-x61/src/lib.rs:12-20` exports `Board` and board modules; hardware facts/config live in `boards/lenovo-x61/src/config.rs:21-108`, board hooks/ACPI/SMBIOS in `boards/lenovo-x61/src/mainboard.rs`, SMM handler in `boards/lenovo-x61/src/smm.rs`.
- **No artificial X61 mainboard crate / no backups:** PASS. `find boards crates ... '*backup*' ... 'fstart-mainboard-lenovo-x61'` returned no matches; `jj diff --name-only` has no `*-backup` or `crates/fstart-mainboard-lenovo-x61` paths.
- **Board-local X61 stage adapters removed:** PASS. `boards/lenovo-x61/src/stage.rs:19-32` implements `FirmwareBoard`; `boards/lenovo-x61/src/stage.rs:34-104` implements `Gm965Ich8UefiBoard`; there is no X61 entrypoint or bootblock/ramstage sequencing there.
- **GM965/ICH8 platform recipe owns X61 bootblock/ramstage sequencing:** PASS. `crates/fstart-platform-intel-gm965-ich8/src/recipe.rs:27-50` defines `Gm965Ich8UefiRecipe`; bootblock flow is `recipe.rs:136-193`; ramstage flow is `recipe.rs:298-405`.
- **Generic selected-board wrapper path, no GM965 branch:** PASS for the intended X61 slice. `xtask/src/build_board.rs:390-399` routes unknown recipes to `write_firmware_board_stage_wrapper`, and generated X61 wrapper aliases `fstart-board-selected` then calls `run_board::<Board>` (`target/fstart-build/lenovo-x61/selected-stage/Cargo.toml:37-39`, `target/fstart-build/lenovo-x61/selected-stage/src/main.rs:37-43`). PARTIAL for future non-x86 recipes due the x86 hard-code noted above.
- **Fixed handwritten stage flow:** PASS for X61. The reusable flow is in `crates/fstart-stage-runtime/src/fixed_flow.rs:65-176`, and the platform recipe supplies concrete `StageFlow` implementations. FAIL as a reduced repo diff until the legacy `StaticBoard` callers are handled.
- **No broad Q35/Pineview/Foxconn/Sunxi migrations in the diff:** PASS by file list (`jj diff --name-only` only touches X61, GM965, stage/runtime, xtask/build-plan, Cargo files, and handoff). But the shared API break still impacts those boards.
- **ACPI path approach:** PARTIAL. Moved to board + path object, but not topology-derived.
- **SMM path approach:** FAIL relative to docs. Board handler exists but is not wired through selected-board SMM flow.

## 4. Fresh-start recommendation

Do **not** fresh-start the whole repo. The current reduced diff is salvageable and finally points in the right architectural direction for X61. Fresh-start only the next slice if needed: make it one narrow cleanup slice, not another broad board migration.

## 5. Minimal next actions

1. Fix the blocker: either restore the old `StaticBoard`/`run_static_board` API until a full legacy-stage deletion/migration slice, or include the full deletion/migration intentionally. Do not leave unrelated boards broken by an X61 slice.
2. Keep this X61 recipe shape; do not add a GM965-specific `xtask` wrapper branch back.
3. Defer SMM wiring and topology-derived ACPI paths to separate slices; track them as doc mismatches, not as proof the X61 recipe slice failed.
4. If claiming wrapper genericity beyond X61, remove/parameterize the hard-coded `fstart-platform-x86_64` dependency in the selected-board wrapper.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Concrete severity-labeled findings cite paths and lines, including the legacy board build blocker, SMM mismatch, ACPI path mismatch, and wrapper genericity limitation."
    }
  ],
  "changedFiles": [
    "handoff/x61-recipe-slice-architecture-review.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "jj root && jj status && jj diff --summary",
      "result": "passed",
      "summary": "Confirmed jj repo and current working-copy changed files."
    },
    {
      "command": "jj diff --stat",
      "result": "passed",
      "summary": "Reviewed reduced diff size and touched files."
    },
    {
      "command": "find boards crates -maxdepth 2 (backup/mainboard check)",
      "result": "passed",
      "summary": "No backup dirs or crates/fstart-mainboard-lenovo-x61 found."
    },
    {
      "command": "rg -n \"run_static_board|StaticBoard\" xtask/src/build_board.rs crates/fstart-stage crates/fstart-stage-runtime boards --glob '*.rs' --glob '!target/**'",
      "result": "passed",
      "summary": "Found legacy board references to removed StaticBoard/run_static_board names."
    },
    {
      "command": "cargo check -p fstart-board-foxconn-d41s --features stage --target x86_64-unknown-none",
      "result": "failed",
      "summary": "Confirmed unrelated legacy board stage build is broken by removed StaticBoard/run_static_board API."
    },
    {
      "command": "nl -ba target/fstart-build/lenovo-x61/selected-stage/Cargo.toml && nl -ba target/fstart-build/lenovo-x61/selected-stage/src/main.rs",
      "result": "passed",
      "summary": "Verified generated X61 wrapper aliases fstart-board-selected and calls fstart_stage::run_board::<Board>."
    },
    {
      "command": "rg -n SMM/smm paths in xtask, X61, GM965, fstart-smm-stage",
      "result": "passed",
      "summary": "Verified X61 SMM handler exists but selected-board SMM wiring is not aligned."
    }
  ],
  "validationOutput": [
    "Intended X61 static typed BSP -> GM965/ICH8 recipe -> selected-board wrapper -> fixed handwritten flow: PASS for X61 path, PARTIAL for generic wrapper, FAIL for reduced-diff mergeability due unrelated board build regression.",
    "cargo check failure: boards/foxconn-d41s/src/stage/bootblock.rs:9 unresolved import fstart_stage_runtime::StaticBoard; boards/foxconn-d41s/src/stage/mod.rs:39 run_static_board not found."
  ],
  "residualRisks": [
    "SMM remains central/platform-selected and does not use the X61 board-owned SMM handler.",
    "ACPI paths are still hard-coded strings behind Gm965Ich8AcpiPaths, not topology-derived AcpiContext.",
    "Selected FirmwareBoard wrapper is still x86-specific despite no GM965-specific branch."
  ],
  "noStagedFiles": true,
  "diffSummary": "Reduced X61/GM965 recipe slice plus shared stage/runtime/xtask plumbing and sha2 force-soft workaround; no broad unrelated board source migrations, but shared API rename breaks legacy board stage builds.",
  "reviewFindings": [
    "blocker: crates/fstart-stage-runtime/src/lib.rs:11 and crates/fstart-stage/src/lib.rs:94-101 remove StaticBoard/run_static_board while boards/foxconn-d41s/src/stage/bootblock.rs:9 and boards/foxconn-d41s/src/stage/mod.rs:39 still require them; verified cargo check failure.",
    "high: boards/lenovo-x61/src/smm.rs:12-29 SMM handler is board-owned but unused; crates/fstart-smm-stage/src/lib.rs:80-82 still uses NoBoardSmmHandler for lenovo-x61.",
    "medium: crates/fstart-platform-intel-gm965-ich8/src/lib.rs:52-115 hard-codes ACPI namespace paths instead of deriving them from topology.",
    "medium: xtask/src/build_board.rs:955-959 and 1017 hard-code x86_64 in the supposedly generic FirmwareBoard wrapper.",
    "low: boards/lenovo-x61/src/mainboard.rs:178-183 still depends on the generic fstart_services::Southbridge concept."
  ],
  "manualNotes": "No lens_diagnostics was run. Repo-wide fresh start is not recommended; the current diff is salvageable after the blocker is fixed."
}
```
