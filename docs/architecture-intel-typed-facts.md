# Intel/X61 typed facts acceptance

Correction to `ab52d16c` (the completed X61 metadata cutover). This is an Intel/X61
migration, not a QEMU schema migration or Cargo workspace/lock ownership cutover.

## Ownership

- `boards/lenovo/x61/src/config.rs`: const-validated factory IFD map, separately
  declared physical chip capacity and CPU population. VBT and chipset/device
  configuration remain board Rust. The facts module is active in firmware and
  editor graphs, not hidden behind a host feature.
- `crates/platform-intel/src/{facts,host}.rs`: small facts contract, shared family
  reservation calculation, and explicit `BuildSelection` resolution. The platform
  selects target, default/allowed payloads, terminal stage, stage bundles and SMM
  features. The i945 geometry test changes its real CAR window through the same
  calculation; it does not migrate an i945 board or claim a new IFD port.
- `crates/image-build/src/{plan,intel_plan}.rs`: selected-plan transport and
  reservation/linker-descriptor projections. The core IFD validator is shared by
  const authoring and decoded transport; transport only adds bounded decoding.
- `tools/fbuild/src/host_plan.rs`: generic Cargo-owned adapter calling
  `platform::Plan::<board::Board>::emit(selection_json)`. fbuild qualifies actual
  dependency aliases, validates Cargo references, normalizes input paths and
  orchestrates existing Intel linker/assembler/SMM backends. It does not choose
  the platform's target, payload policy or stage feature bundles.

X61 has no `runtime`, `stage`, `smm`, or payload forwarding features, direct stage
runtime dependency, or board host executable. The platform's hygienic entry and
SMM exports retain the handwritten flows. ACPI is in the normal ramstage bundle;
Lenovo EC/PMH7 initialization is still independent of table emission. No register
sequences, authenticated loader chain, stash ABI, or trained-RAM checks changed.
Runtime geometry still comes from the retained linker descriptor.

## Packaging boundary

The existing selected-source preparation **still resolves a root-seeded lock**.
After preparation, only a deterministic local runner entry is appended, retaining
all prepared package entries/pins verbatim. Adapter compilation uses `--locked`
and fails closed without a resolver fallback. Thus the adapter's guarantee is
not a claim of end-to-end zero-resolution builds.

The root lock changes only four local dependency edges: image-build → serde;
platform-intel → image-build, serde_json, smm. No external versions, sources or
checksums change. Board locks remain intact. Manual rustc linking was rejected:
it would duplicate Cargo's panic/profile, native-link and artifact handling.

## Validation

Reference configuration: release. The final focused suite passes 54 tests
(30 fbuild, 11 core, 9 image-build, 4 Intel). Representative commands:

```sh
cargo test --locked -p fbuild -p fstart-core -p fstart-image-build \
  -p fstart-platform-intel --features fstart-platform-intel/host
cargo run --locked -p fbuild -- assemble -b lenovo-x61 --release --payload halt
cargo run --locked -p fbuild -- assemble -b lenovo-x61 --release --payload uefi
cargo run --locked -p fbuild -- ide lenovo-x61 --release --payload uefi --stage ramstage
cargo run --locked -p fbuild -- build -b intel-d945gclf --release
cargo run --locked -p fbuild -- build -b foxconn-d41s --release
cargo run --locked -p fbuild -- assemble -b qemu-riscv64 --release --payload halt
```

Evidence retained in the implementation environment:

| Proof | Evidence |
| --- | --- |
| Focused tests, including actual fresh/cached `--locked` adapter dependency aliases | `/tmp/fstart-typed-selection-tests.log` |
| X61 halt/UEFI links and assembly | `/tmp/fstart-typed-x61-final-{halt,uefi}.log` |
| Baseline geometry, linked descriptors, exact current SMM and microcode | `/tmp/fstart-typed-image-proof.json` |
| Original-source live RA car → postcar → ram switching, macro expansion and overlapping descriptor E0080; restored compiler check | `/tmp/fstart-typed-editor-proof.log`, `/tmp/fstart-typed-editor-proof/` |
| Valid one-page ME/BIOS boundary change reaches all stage descriptors and assembler; source restored | `/tmp/fstart-typed-fact-change.log` |
| Actual selected board/platform host graph, fresh/cache-hit receipts and unchanged prepared lock | `/tmp/fstart-typed-cargo-host-proof-final.log` |
| Legacy Intel and QEMU release regressions | `/tmp/fstart-typed-legacy-{i945,pineview}.log`, `/tmp/fstart-typed-qemu-riscv64.log` |
| Local-only lock diff | `/tmp/fstart-typed-lock-diff.txt` |

The comparison verifies the complete effective reservation model and identical
linked descriptor bytes for bootblock, postcar and ramstage. The exact current
11,856-byte SMM image occurs once in each ramstage. The same 86,016-byte microcode
blob occurs in baseline and new FFS images. Full firmware binaries need not be
byte-identical after compiler/dependency/adapter changes; this is not a hardware
boot or stack high-water measurement. No full board matrix or new QEMU boot claim.

The live editor probe reuses `ci/ide-tests.py`'s bounded LSP client. It checks
canonical original-source definitions, stage/payload cfg switching and the
platform entry macro, then temporarily moves the descriptor into the GbE range.
Both compiler and RA report E0080 at the original board URI. The source is
restored in a `finally` block and the exact editor compiler command passes again.
Generating a new plan/view requires valid host facts; an existing view reports
source errors without regenerating the plan.

## Distinct IFD active-list fix and flashing safety

The metadata baseline explicitly declared descriptor/GbE/ME/BIOS, but conversion
used `ConstVec::new(first)` without pushing `first`. Its serialized active list
therefore omitted the descriptor. Typed X61 facts explicitly push all four, so
descriptor overlap participates in the shared validator. This is a separate
active-list bug fix, not a byte-identical restoration of that serialized list.

Both payload comparisons find the entire non-BIOS range `[0, 0x280000)` unchanged:
2,621,440 bytes of erased `0xff`. The assembler was not changed to synthesize,
import or replace opaque IFD/GbE/ME contents. The generated 4-MiB firmware image is
**not a populated factory/full-chip backup and must not be blindly flashed over
the entire hardware chip**. Hardware boot and safe board-specific deployment
remain separate gates.
