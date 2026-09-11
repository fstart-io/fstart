# Concrete platform build-plan boundary

This corrects the tooling boundary left by [Intel typed facts](architecture-intel-typed-facts.md).
That change moved hardware facts and feature policy into Rust, but fbuild still
selected Intel roles, SMM consumers, linker entry points and ELF checks. Merely
moving its Intel executor would not have fixed the boundary.

## D945GCLF common-plan migration

D945GCLF and its postcar-debug variant now use the platform-owned Intel host
export, bundles, stage adapter and linked runtime descriptors. Physical layout
transport distinguishes `IntelIfd` from `X86Legacy`; D945 retains its real
512-KiB contiguous flash, without a synthetic descriptor. Shared platform policy
owns RAM/CAR reservations and actual-size XIP placement. The old board host
executable, capability relays and duplicate i945 stage geometry are removed.
Diamondville microcode and the current-artifact ICH7 SMM producer remain real.
X61 adopts only the generalized physical-layout wrapper; D41S was migrated
in the final batch (see Final board migration and legacy-route removal).

A separate correction implements `IntelEcamConfig` for `IntelI945Config`,
reporting the same configured base used by existing PCIEXBAR programming.
DesktopGc uses `F0000000` with 64 buses. No hardware register sequence changes.
The old canonical UEFI payload now compiles/links, but packing correctly fails:
FFS length `0xc80d8` overlaps the actual bootblock `[0x5f000..0x80000)`.
There is no flash enlargement or fixed per-stage storage slot to hide this.

Hardware evidence: **Intel Desktop Board D945GCLF Technical Product
Specification**, April 2008, E35968-001US revision -001, identifies 82801GB ICH7
and, in §3.2, a 4-Mbit SPI device. **Intel I/O Controller Hub 7 Family
Datasheet**, April 2007, 307013-003, §12.3 explicitly excludes 82801GB from AHCI;
§10.1.33 defines FDVCT bit 3 as AHCI disabled when set. This is documentary
silicon evidence, not an observed board FDVCT read. The existing `Ahci` request
is a baseline board-policy bug, deliberately retained for a separate correction.
No SATA boot claim is made; an ATA driver/host block device is separate work.
USB2, PS/2 and serial policy are preserved.

Fresh receipts under `/tmp/fstart-d945-*` cover 106 tests, seven double-identical
release selections, independent signature/directory/physical-copy verification,
ELF descriptors and load bounds, exact SMM embedding, two representative virt
boots, fresh/cached locked host graphs and live D945 stage-to-AArch64 editor
switching. `plan.md` records primary URLs, baseline and effective budgets;
`images/proof.json`, `auth.json`, `boots/proof.json`, `host-proof.log` and
`editor-proof.log` are the detailed software evidence, not hardware boots.
`debug-proof.json` checks the six raw COM1 checkpoint encodings: absent in the
normal image and present in debug, matching the accepted baseline selections.

Variables and the new CrabEFI API/pin are **not implemented in this step**.
`InputRegion::Raw` reserves bytes and records metadata without raw-content
hashes; `ExternalRaw` only describes an extent and cannot by itself prevent
packing over it. A variable partition still needs chip/erase evidence, a
persistent-capacity decision and bounded backend integration. No partition is
allocated from guessed RAM-cache capacity or apparent free flash. Mutable
contents must stay outside FFS content authentication; approved local FV-header
checksums/journal CRCs remain. Only a fully erased region may autoformat;
nonblank corruption and read/protection failures must be preserved and reported,
not silently converted to volatile storage. Post-EBS durable NV policy remains
unresolved. No update, power-fail-safety or A/B claim is added.

## Locator/trust separation (phase2a)

The initial-only locator is wire version 7, 40 bytes. It preserves x86's
`anchor_offset`/microcode offset/size fields at 24/28/32. The separate 320-byte
`FSTRUST1` policy holds expected keys, family and an explicit minimum version
(currently zero, **not** persistent anti-rollback). Expected policy is linked
into a verifier protected by the platform/predecessor trust boundary, never
selected from an image header or handoff. Media policy scanning is a `std`-only
host consistency facility, unavailable to firmware callers. Host decoding is
by-value little-endian parsing, independent of buffer alignment. Current mounts and
root entry reject nonzero image origins; slot-relative selection remains phase2b.

The builder finalizes constant policy before compression, rejects compressed
locators and stale prepatched policy, then packs actual extents. Its explicit
`FfsImage::trust_bytes` output supplies separate physical reset copies; their
policy is never selected by scanning the packed image. Initial locator
fields and Sunxi's successor pin can then be patched without changing any
compressed input. The fixed Sunxi callback computes eGON checksum, hashes the
complete final SPL (including locator/checksum), and reseals the fixed-length
directory and ROOT512. There is no compression/layout convergence loop. Existing
root and directory formats remain; this is not root-first slots or phase2b A/B.

`*.inputs.txt` identifies **pre-compression constant-policy-patched inputs** by
SHA256 and policy hash. Compiler artifacts retain their unpatched identity;
there is no separately cached compression result to reuse across policy changes.
These input identities are not presented as final initial-stage hashes: final
locator/pin/checksum bytes are covered by the published directory's file hashes.

Sunxi's version-2 handoff has a fixed magic/version/body-length header bounded by
the unchanged 256-byte reservation. Mainstage checks the configured pointer;
ARM late media consumers validate carried image/root/microcode bounds against
initialized MMC capacity before publishing an immutable locator snapshot. SPL
also checks real device bounds but remains hash-pin-only: no early directory or
Ed25519 dependency, raw fallback, or retroactive self-authentication claim. D1's
existing halt-only mainstage is not turned into a Linux/FFS consumer here.

Intel's version-2 retained stash appends the 40-byte locator without moving the
assembly MTRR prefix or changing the reserved-page lifetime. Mainstage intersects
it with independently linked firmware bounds and imports locator/directory
metadata before publishing the mounted context. Mounting an uninstalled or
invalid locator fails instead of silently publishing an empty context. These
prerequisite phases do not initialize hardware. APs reuse the bounded microcode
span. Microcode is also a signed-directory file, but the existing pre-Rust/AP
access path performs vendor validation, **not** directory digest verification.
This change does not add verification work or alter early/AP hardware ordering.

RISC-V virt now packs its initial stage and subsequent files into one physical
32-MiB bank; there is no artificial 16-MiB separation. The linked descriptor
bounds the bank, the locator bounds the actual mounted image, and payload
consumers reuse that exact mounted extent. Physical pflash output is still
32 MiB. ARMv7/AArch64 retain their two physical 64-MiB banks, reset placement and
AArch64 relocation contract. Legacy QEMU consumers retain their full-window
contract until changed together; this is not a migration of those boards.

Final evidence lives outside the repository under `/tmp/fstart-phase2a-review-*` (superseding pre-review receipts):

- `final-images/proof.json`: 21 release selections assembled twice with identical
  FFS, physical images and input identities; immutable per-selection snapshots.
- `final-auth.json`: 21 board-key Ed25519/directory checks, bootstrap stored/loaded
  SHA256 checks and 57 independently verified loaded-file hashes. External Intel
  initial files are checked against actual physical BIOS bytes, not a short FFS
  buffer. Separate physical reset-copy policy bytes also match the builder's
  explicit policy identity. Sunxi checksum, complete SPL digest, pin and hash-only symbols pass.
- `final-boots/proof.json`: eight fresh virt halt/Linux/UEFI boots from those
  exact physical bytes. `negative/proof.json`: seven fresh rejection boots for
  signature, directory, payload and malformed/out-of-bank locator mutations.
- `final-tests.log`, `final-ffs-tests.log`, `final-stage-tests.log`,
  `final-ram-test.log`: 65 + 25 + 13 + 1 passing tests. These include policy changes
  that change compressed extent, repeat identity, stale policy rejection and
  malformed transport bounds/version/length; explicit physical-policy copying
  despite a decoy media record; unaligned host parsing; and real `ram`-cfg
  pre-import mount rejection followed by working microcode and verified assets.
- `host-proof.log`, `editor-proof.log`: fresh/cached locked host graphs and live
  Intel car/postcar/ram/SMM → AArch64 cfg, navigation, macro and compiler views.
- `final-lock-proof.json`: original root/eight standalone locks and file set
  unchanged, no external pin or board-source edits, no temporary linker override.

These are software/emulator checks, not Intel/Sunxi hardware boots, stack
high-water measurements, secure boot or rollback. D945 UEFI's baseline missing
ECAM trait implementation remains outside this phase. The phase1 measurements
below are historical; its deferred locator loop and RISC-V split are superseded.

## Packed-storage correction (phase1 historical evidence)

The fixed-storage premise in the historical acceptance below is retracted.
Runtime load/BSS/stack/heap windows remain fixed and validated; flash files do
not receive arbitrary preallocated slots. X61 now links its initial XIP bytes
at the BIOS top using section-size arithmetic in one link. The full BIOS region
is its legal address window; actual linked bytes determine the space available
to the existing packer. Reset-vector addresses, physical flash/BIOS identity and
MTRR policy are unchanged. RAM-stage `.data` is writable in place immediately
after initialized code/rodata (`VMA = LMA`); `.bss`, heap and stack remain in their
separate protected runtime reservation. There is no new startup copy or ABI.
The common `flat_exact_size` field/check is removed; maximum load bounds remain.

Matched release measurements, captured before this correction:

| X61 stage / payload | Before flat bytes | After flat bytes |
| --- | ---: | ---: |
| Bootblock, both payloads | 262,144 | 126,976 |
| Postcar, both payloads | 20,740 | 20,744 |
| Ramstage, halt | 4,194,312 | 145,944 |
| Ramstage, UEFI | 5,334,792 | 2,500,224 |

The four-byte postcar difference is alignment, not a removed large gap.
Bootblock PT_LOAD initialized bytes remain 115,088; the flat includes required
alignment/reset-page placement. FFS halt shrinks 217,253 → 201,373 bytes and UEFI
889,829 → 878,621: compression previously hid much of the zero padding on media,
but not in the authenticated/decompressed initialized extent. These are matched
payloads, not the invalid comparison of historic halt against current UEFI.

Evidence: `/tmp/fstart-packed-baseline/`, `/tmp/fstart-packed-x61-{halt,uefi}.log`
and `/tmp/fstart-packed-image-proof.json`. The artifact proof checks unchanged
postcar/ramstage descriptor bytes, identical current SMM embedded once, identical
86,016-byte microcode and 2,621,440 erased non-BIOS bytes. The latter remain
**generated erased content, not a factory/full-chip backup**.

All supported three-virt release payload assemblies and named X61
bootblock/postcar/ramstage/SMM plus virt check/IDE commands pass:
`/tmp/fstart-packed-regression.log` and matching directory. Eight fresh virt
emulator boots pass (`/tmp/fstart-packed-boots.log`); no Intel hardware boot.
ARMv7/AArch64's two 64-MiB flash banks are real backing devices, **not** an
arbitrary within-bank half split. They remain unchanged. RISC-V's artificial
16-MiB split inside one 32-MiB bank is deferred to the locator-transport phase.

The compressed-anchor iteration is deliberately still present. AP microcode
lookup and late block-media mounts still consume locator-bearing anchors; a
stash-backed directory alone does not eliminate those dependencies. Splitting
constant trust from mutable location needs a separate audited consumer change.
No root-first/A/B format, updater, raw Sunxi mode or D945/D41S migration is claimed
here. In particular this phase does not claim acyclic signature finalization,
power-fail safety, secure boot, stack high-water or a full board matrix.

Additional gates: the 61 focused tests pass, including real-LLD initialized
storage growth/shrink, BSS growth without media growth, reset placement and
capacity rejection (`/tmp/fstart-packed-final-tests.log`). Fresh/cached actual
`--locked` Intel/AArch64 board/platform/runner graphs pass with original sources
and unchanged prepared locks (`/tmp/fstart-packed-host-proof.log`). Root and all
eight existing standalone locks were snapshotted before Cargo/LSP work and are
byte-identical, with no new standalone files:
`/tmp/fstart-packed-locks-1788881097938681837/manifest.json` and
`/tmp/fstart-packed-final-lock-proof.json`. No external pins changed. Both corrected X61 halt/UEFI images are byte-identical
to a later reassembly (full flash and FFS), recorded in
`/tmp/fstart-packed-determinism.json`. This demonstrates deterministic outputs,
not absence of the still-retained compressed-anchor iteration.

The remaining gates passed after the parent restored the **exact original** Nix
glibc/zlib/rustup-wrapper closures with `nix-store --realise`. An intervening
missing-loader failure is retained in the conversation; no compiler replacement
or global binary patch was used. Live RA now passes Intel car → postcar → ram →
SMM → AArch64 switching, original-source definitions, cfg and hygienic macro
views, plus the corresponding compiler commands
(`/tmp/fstart-packed-editor-proof.log`). No on-disk source probes remain.

Legacy release assemblies pass for D945 halt/debug variant, D41S halt/UEFI,
Banana Pi ARM and LicheeRV RISC-V, with locks checked after every selection
(`/tmp/fstart-packed-legacy.log`). D945 UEFI's pre-existing missing ECAM trait
implementation is not fixed or claimed supported. Both Sunxi SPLs remain
24,576-byte eGON images (20,580/20,148 initialized PT_LOAD bytes respectively),
with SHA pin support but no FFS/Ed25519/directory compiler features or linked
verification symbols. Independent pin SHA256, eGON checksum and final initial
file digest checks pass (`/tmp/fstart-packed-sunxi-proof.json`). Their images
match the pre-rebuild retained files; those files are **not** claimed as fresh
matched baselines. No new raw-loader mode or secure-boot claim is introduced.

Twenty-one FFS tests pass (`/tmp/fstart-packed-ffs-tests.log`). Fresh RISC-V Linux
negative boots reject root-signature corruption, directory corruption and
compressed payload corruption at the expected verification gates, without
reaching successful payload boot (`/tmp/fstart-packed-negative/proof.json`).
This is existing single-image integrity behavior, not A/B recovery. Sunxi's
eGON checksum remains finalized before the SPL digest; no finalizer source was
changed in this correction.

## Migrated scope

**X61 and QEMU RISC-V, ARMv7 and AArch64 use the same concrete plan and executor.**
This is not a claim that all of fbuild is generic. At the time, remaining
board-host/BoardConfig builds, including i945 and Pineview, retained their
earlier legacy path; all have since been migrated and the legacy path deleted
(see Final board migration and legacy-route removal). None of
these three virt boards has a Cargo geometry recipe beside its Rust preset.

- `crates/image-build/src/build_plan.rs`: family-free `BuildPlan`, named
  `CompilationUnit`s, explicit Cargo target kind/triple, entry/environment/payload
  cfg values, features, compiler flags/build-std, linker text, ELF expectations,
  generated-artifact bindings and assembly projection.
- `crates/platform-intel/src/host.rs`: platform-owned conversion into four units.
  The SMM library unit explicitly selects release/PIC compilation and its format
  operation. Ramstage explicitly binds that producer's current image/header.
  The executor does not discover that relationship from a role or name.
- `crates/platform-qemu/src/{facts,host}.rs` and the three virt board configs:
  unconditional typed `VirtMachine` selection and hardware config. The platform
  owns invariant flash banks, reservations and compiler bundles. One projection
  composes explicit RISC-V XIP, ARMv7 XIP/direct Linux and AArch64 relocation
  policy; it is not three copied constructors or a lifecycle DSL. AArch64 storage
  starts at zero, execution at `0x40400000`, writable storage at `0x40800000`.
  No producer, CAR flow or Intel entry convention is imposed.
- `tools/fbuild/src/{host_plan,plan_executor,selection}.rs`: existing Cargo-owned
  host runner, alias qualification, bounded dependency execution and one compiler
  selection used by build, check and IDE. The old `intel_build.rs` and
  `resolved_intel.rs` executors are removed.
- `crates/image-build/src/{linker,elf,intel_assembly}.rs`: reusable linker/ELF/image
  format mechanisms. Platforms select helpers and emit their concrete results;
  the common tools do not select a platform helper by family identity.

`UnitOutput::SmmImage` is an existing image-format operation, not a platform
identity: target, Cargo library selection, cfgs, flags, features and consuming
bindings are separate explicit fields. Its handler is linked from **only the
current Cargo JSON rlib artifacts**. Executables likewise come from Cargo JSON,
not guessed shared target paths. Build/check receipts are retained beside each
compiler selection. Whole-plan identity includes generated linker text and
compiler options; consuming directories additionally hash length-delimited
producer identities and actual file bytes.

## Bounded dependencies, not an authored workflow

The plan has at most 32 units. It rejects duplicate/invalid unit IDs, missing
producers/artifacts, duplicate environment bindings and cycles before launching
commands. Named selection takes only the selected unit's transitive producer
closure. Prerequisites are built even for check/IDE; the selected unit is then
built, checked or analyzed with the same compiler selection. Whole-image check
builds required producers and checks non-producers.

Unit names are safe path components. Environment names follow portable
identifier rules, reject compiler-flag overrides and NUL values, and are not
validated as filesystem names. Each platform emits an independent allowed cfg
schema; selected entry/environment/payload values must belong to it. Common
selection generates check-cfg flags from that schema, with no tooling-owned value
registry and no automatic whitelisting of the selected value. A real rustc test
accepts novel platform vocabulary under `-Dunexpected_cfgs` and rejects a typo.
The pre-resolution host runner selects none of these firmware cfgs: it registers
only their names with `values(any())`, so inactive new board guards compile
without a tooling vocabulary. An actual fresh/cached Cargo fixture uses novel
guards under `deny(unexpected_cfgs)` and asserts no firmware branch is selected;
the emitted target units still use strict independent schemas.

Every plan artifact binding key is removed from units that do not bind it. Direct
Cargo commands and metadata/audit use the same environment application helper;
IDE check commands encode equivalent `env -u` removals, while bound values
explicitly overwrite ambient ones. Preparing a common selection also rejects
unrecorded ambient `FSTART_*` firmware inputs, including producer keys from other
plans (`FSTART_WORKSPACE_ROOT` remains tool configuration). The editor graph
records compiler/build-script environments, not an inferred producer namespace.

Payload input bindings are the bounded existing kernel/firmware/FIT slots; unknown
or duplicate slots and zero capacities fail both at plan validation and at the
input-validation entry point. Unsupported required file kinds cannot be silently
ignored. There are no arbitrary commands,
board-authored DAGs, lifecycle steps or new RON/TOML authoring schemas.

```sh
fbuild build -b lenovo-x61 --release --payload halt --stage smm
fbuild check lenovo-x61 --release --payload halt --stage postcar
fbuild ide lenovo-x61 --release --payload uefi --stage ramstage
fbuild ide qemu-aarch64 --release --payload halt --stage stage
```

Stage names are strings resolved against the selected plan. No CLI Intel enum
or IDE ramstage/SMM special case remains. A multistage IDE view requires a named
unit; a one-stage image defaults to its sole image unit. Unknown names fail
rather than silently selecting a different stage.

## Assembly and remaining limits

`Assembly` is a projection into the existing assembler, **not a source of
compiler policy**. It carries memory/image placement, explicit IFD format data,
stage compression/load relationships, security, payload/microcode inputs,
SoC image format and boot-hart ID. Authenticated-bootstrap wire roles are explicit
bindings supplied by the platform: the common assembly path never infers
Postcar/Mainstage from a compiler unit name. Name inference exists only for
explicit legacy callers without these bindings.

The shim supplies no runtime CAR, ACPI, SMBIOS or SMM objects: those are not
assembler inputs and remain owned by the firmware/platform flow. It currently
projects raw memory-mapped and Intel-IFD images; this is not a universal flash
partition or SoC image framework. Adding genuinely new image/compiler mechanisms
can require library support. Adding a platform using the represented mechanisms
requires no new fbuild family enum, match arm or CLI branch. New mechanisms must
not be smuggled in as platform identity tests in the executor.

Existing register sequences, auth/stash/handoff ABI, reset relocation code and
runtime descriptor authority are unchanged. Intel's full BIOS mapping/MTRR
identity remains distinct from FFS capacity. ACPI remains normal ramstage policy,
independent of EC initialization. Fixed **runtime** capacities do not adapt to
linked sizes; stored file extents do. Intel's initial XIP image now self-sizes at
the BIOS top in one link, and RAM-stage initialized data is contiguous rather
than spanning the gap to its BSS reservation. The full BIOS is the legal XIP
address window, not a preallocated bootblock slot. ELF validation checks
architecture/class/endianness, PT_LOAD physical and virtual
bounds, entries, linear copy extents, symbols and exact retained descriptor bytes.
The descriptor's file bytes must actually be covered by its load mapping.

## Cargo/editor contract

The selected-source preparation and lock ownership are unchanged. The root lock
adds only local edges QEMU → image-build/serde_json and fbuild's test-only QEMU
host dependency; external versions/sources/checksums are unchanged. Board locks
are retained. The generated runner still appends one deterministic local package
entry to its prepared lock and builds with `--locked`, with no fallback. This
continues to be a guarantee **after** the pre-existing selected-source preparation,
not a claim of end-to-end zero-resolution builds.

All three virt boards use the same platform-owned hygienic stage export pattern
as Intel. Their machine selection and hardware config live in original Rust
source, not behind a host-only feature. The platform owns shared firmware bundles;
the boards have no forwarding recipe, direct stage runtime dependency or host
executable. AArch64's former board-authored fixed flash capacity was removed:
it was a machine invariant, not demonstrated supported variation. Editor projects remain Cargo-derived, target-bounded and
canonical original-source graphs.

## RISC-V / ARMv7 virt follow-up batch

The platform presets preserve these fixed reservations (bytes):

| Machine | Flash / split | Writable | Stack / heap | Linux kernel | Payload runtime / DTB |
| --- | --- | --- | --- | --- | --- |
| RISC-V | `0x20000000`, one packed 32-MiB bank | `0x81000000`, 4 MiB | 1 MiB / 256 KiB | `0x82000000`, 64 MiB | OpenSBI `0x80100000`, 2 MiB; DTB 64 KiB at `0x87f00000` (Linux) or `0x80f00000` (UEFI) |
| ARMv7 | `0`, 128 MiB / 64 MiB | `0x40200000`, 1 MiB | 256 KiB / 256 KiB | `0x41000000`, 64 MiB | No external firmware; DTB `0x40f00000`, 64 KiB |

Halt has no payload/kernel/runtime/DTB inputs. ARMv7 supports halt and direct
Linux, rejects UEFI, and never requests an external firmware file. RISC-V keeps
halt/Linux/UEFI, its distinct payload DTB destinations, bounded OpenSBI DTB growth,
authenticated loading and MP/handoff behavior. Hardware flows and entry/loader
ABIs are unchanged; only the stale RISC-V module description was updated.

The last production consumers of `riscv64-xip` / `armv7-xip` Cargo geometry are
gone. The bounded follow-up has now deleted the metadata schema/resolver,
`ResolvedImage::Legacy`, `resolved_build.rs`, metadata layout overrides and the
old `Selection::prepare/prepare_stage` route. All build-profile references name
`rust`. The ten boards still on the separate **BoardConfig host-callback
path** at the time have since been migrated and that path deleted (see Final
board migration and legacy-route removal). Direct Cargo feature validation now lives in the
narrow `cargo_features` module. Shared BoardConfig linker helpers and its cfg
vocabulary remain; the common executor still consumes platform-owned schemas.

Five independently captured pre-removal output fixtures live in
`tools/fbuild/src/fixtures/qemu-xip/`. Exact descriptor bytes (hex), complete ELF
expectations and assembly JSON are inspectable; linker SHA256 fingerprints avoid
repeating hundreds of descriptor BYTE lines. The original resolver and the new
golden test were checked equivalent before deletion. No production resolver or
metadata recipe is retained for tests. Real LLD tests still select typed presets
for all three architectures, including negative fixed-capacity/copy-extent checks.

The selected machine and handwritten runtime program remain separate Rust
selections: selecting the wrong pair fails later when compiling the target
program, not in a unified type-level constructor. No lifecycle abstraction is
introduced to hide that coupling. Shared XIP/ELF validation now rejects
unrepresentable 32-bit reservations, including descriptor-only payload regions,
expected entries/symbol addresses and actual PT_LOAD/symbol extents. A range may
end exclusively at `2^32`; an entry or emitted symbol (such as `_stack_top`) may
not have that value. Checked arithmetic also rejects 64-bit extent overflow.
This is format validation, not a chipset-specific checker.

Ten legacy packages remained for subsequent batches at the time (no migration
claim then; all have since been migrated, see Final board migration and
legacy-route removal):
`qemu-q35`, `qemu-sbsa`, `qemu-sifive-u`, `sifive-unmatched`, `intel-d945gclf`
(i945), `foxconn-d41s` (Pineview), `bananapi-m1`, `orangepi-r1`, `orangepi-pc2`,
and `licheerv-dock` (the four Sunxi boards).

### Batch acceptance commands and evidence

The bounded command/probe scripts are `/tmp/fstart-qemu-batch-{builds,finish}.sh`.
Representative commands (kernel/firmware inputs are explicit):

```sh
cargo test --locked -p fbuild -p fstart-core -p fstart-image-build \
  -p fstart-platform-intel -p fstart-platform-qemu \
  --features fstart-platform-intel/host,fstart-platform-qemu/host
fbuild assemble -b qemu-riscv64 --release --payload linux \
  --kernel boot-assets/payloads/Image-riscv64 --firmware boot-assets/payloads/fw_dynamic.bin
fbuild assemble -b qemu-armv7 --release --payload linux \
  --kernel boot-assets/payloads/zImage-armv7
fbuild check qemu-riscv64 --release --payload uefi --stage stage
fbuild ide qemu-armv7 --release --payload linux --stage stage
```

- **64 focused tests pass**, including actual LLD fixed-capacity/ELF range and
  AArch64 copy-extent tests: `/tmp/fstart-qemu-batch-final-tests.log`.
- Final release assembly: RISC-V halt/Linux/UEFI, ARMv7 halt/Linux, AArch64 halt,
  and X61 halt (no hardware boot): `/tmp/fstart-qemu-batch-final-builds.log` and
  `/tmp/fstart-qemu-batch-{riscv64,armv7,aarch64,x61}-*.log`.
- Six fresh post-review boots with existing `ci/qemu-boot-tests.sh`, filtered to
  the affected machines: `/tmp/fstart-qemu-batch-final-boots.log` and the matching
  directory. Linux reaches `FSTART_CI_BOOT_SUCCESS`, RISC-V UEFI reaches
  `Boot manager finished`, halt reaches ready-for-payload/PCI. No ARM UEFI claim.
- Immediate prechange plans for all five combinations and halt assemblies:
  `/tmp/fstart-qemu-batch-baseline/`. Exact descriptor/effective ranges and actual
  linked ELF comparison: `/tmp/fstart-qemu-batch-image-proof.json`. AArch64's
  assembly/output geometry also matches its earlier common-plan baseline.
  Full binary equality is not required: ARM halt happens to match exactly;
  RISC-V halt retains the same 79,312-byte flat size and all 119 common sized
  text symbols retain sizes, but 73 move, changing relocations/code bytes and
  signed image content after compiler feature/graph changes. Details:
  `/tmp/fstart-qemu-batch-{byte-comparison,riscv-codegen}.json`.
- Fresh/cached **actual Cargo-reported** host artifacts, canonical original
  sources, no firmware feature/cfg selection, unchanged prepared runner locks,
  and selected external pins equal to root:
  `/tmp/fstart-qemu-batch-host-proof-final.log` and matching directory.
- Live RISC-V halt → ARM Linux → AArch64 halt → RISC-V UEFI → ARM halt selection,
  original preset/hardware navigation, cfgs, proc macros/hygienic entry and
  named checks: `/tmp/fstart-qemu-batch-editor-proof.log` and matching directory.
  Both new boards produce compiler and live RA E0080 for a genuinely invalid
  typed PCI resource capacity, then recover after source restoration. There is
  no artificial board flash-capacity negative test.
- A temporary **shared platform policy-only** ARM heap change (256 → 192 KiB)
  reaches descriptor, linker symbols, ELF expectations, actual flat image,
  assembly, compiler identity, named check and IDE, then restores the original
  plan exactly: `/tmp/fstart-qemu-batch-policy-proof.log` and matching directory.
  This does not expose a supported board geometry override. All probes restored.

Root `Cargo.lock` is byte-identical to the parent; there are no new root edges.
Eight existing standalone board locks are retained across the fourteen board
packages. These files are ignored/untracked: pre-batch byte preservation of the
RISC-V/ARM standalone locks is **unproven**, and refresh was observed during the
work (cause not established from mtime alone). Do not confuse that limitation
with the verified root/selected-runner lock guarantees. Current standalone locks
were snapshotted under `/tmp/fstart-qemu-batch-retained-locks/`; subsequent probe
changes were restored to that snapshot. No workspace/lock ownership cutover,
benchmark, high-water measurement or full-matrix claim.

## Metadata resolver removal acceptance

This bounded follow-up changes no hardware/runtime program or family migration.

- **61 focused tests pass** with the root `--locked` command above:
  `/tmp/fstart-metadata-cleanup-tests.log`. Relative to the preceding 64, four
  obsolete resolver-only tests and the old compiler-selection test are removed;
  two shared ELF-width tests are added. Feature validation and the editor graph
  proof now use the live common path. Actual LLD and negative overflow tests remain.
- Pre-removal capture and simultaneous old-resolver/new-golden equivalence:
  `/tmp/fstart-metadata-cleanup-{golden-capture,predelete-proof}.log` and
  `/tmp/fstart-metadata-cleanup-goldens/`. The shipped fixtures remove duplicate
  descriptor byte arrays (the same bytes are retained as hex).
- Seven common effective plans (RISC-V halt/Linux/UEFI, ARMv7 halt/Linux,
  AArch64 halt, X61 halt) are byte-identical before/after, and all seven release
  assemblies succeed. i945, Pineview and qemu-sifive-u BoardConfig host releases
  link (pre-migration evidence; those flows are now platform plans). All four common routes pass named `check` and generated Cargo-derived
  `ide` with release selections; no live-editor or hardware claim in this cleanup.
  Commands/logs: `/tmp/fstart-metadata-cleanup-acceptance.sh`, matching `.log`,
  and `/tmp/fstart-metadata-cleanup-after/`.
- Selected compiler receipts, linker text, flags and actual executable bytes
  match the immediate prechange capture:
  `/tmp/fstart-metadata-cleanup-after/selected-artifact-proof.json`. Thirty
  flat/assembled outputs also match a replay with the saved prechange fbuild
  executable and the identical plans; this is an explicit old-executable replay,
  not an invented prechange image snapshot. See `image-hashes-{new,prechange-replay}.json`
  in that directory and `/tmp/fstart-metadata-cleanup-replay.log`. No changed
  common output required another boot; earlier affected-machine boot evidence
  above is not claimed as freshly rerun here.
- Root and all eight existing standalone board locks were snapshotted **before
  any repository/Cargo/LSP inspection** and remain byte-identical:
  `/tmp/fstart-metadata-cleanup-locks/manifest.json`. No new standalone locks.
  Common selected-runner locks also match the prechange capture. The broad
  historic artifact audit records two exceptions outside that common scope:
  qemu-sifive-u's stale host linker output and disposable selected-workspace lock
  were regenerated by its existing halt preparation. This does not change root
  or board lock authority and is not a claim that every historic target file is
  immutable. No workspace/lock ownership cutover or source probes remain.

The ten then-unmigrated BoardConfig host packages listed above have since
been migrated (see Final board migration and legacy-route removal).
No whole-matrix, hardware-boot, stack-high-water or benchmark claim.

## Earlier common-plan acceptance evidence

Evidence is recorded in the implementation environment, not shipped as generated
board configuration. The earlier evidence below is historical: its AArch64
invalid physical-capacity board fact was deliberately superseded by a platform
machine invariant in this batch, rather than preserved as an artificial input.
Final compiler and regression commands for that earlier scope are collected in
`/tmp/fstart-common-final-builds.sh`.

- The original 59 focused tests pass in `/tmp/fstart-common-tests.log`.
  The cfg-schema/environment-isolation review adds three tests (62 total),
  including subprocess-inherited environment checks:
  `/tmp/fstart-common-review-tests.log`.
- Follow-up compiler/editor and actual fresh/cached locked-host checks:
  `/tmp/fstart-common-review-ide.log`, `/tmp/fstart-common-review-live-ra.log`,
  `/tmp/fstart-common-review-host.log`. These supplement the release/boot evidence
  below; the follow-up changes the host contract and environment isolation, not
  hardware or runtime layout policy.
- X61 halt/UEFI and relocating AArch64 halt release assembly:
  `/tmp/fstart-common-acceptance-builds.log`.
- Exact comparison to the typed-facts baseline:
  `/tmp/fstart-common-image-proof.json`: all three descriptors and effective
  ranges match, as do the current 11,856-byte SMM image (embedded once), the
  86,016-byte microcode blob, and 2,621,440 erased non-BIOS bytes.
- Fresh AArch64 emulator boot through integrity, device tree, PCI and
  ready-for-payload: `/tmp/fstart-common-aarch64-boot.log` and
  `/tmp/fstart-common-aarch64-boot-proof.json`.
- Live Intel car → postcar → ram → SMM → AArch64 selection switching, original
  fact navigation, hygienic macro expansion, compiler/RA E0080 for overlapping
  IFD and invalid physical flash capacity, followed by restored compiler checks:
  `/tmp/fstart-common-editor-proof.log` and `/tmp/fstart-common-editor-proof/`.
- Actual fresh/cached locked host Cargo graphs for both platforms:
  `/tmp/fstart-common-host-proof.log` and `/tmp/fstart-common-host-proof/`.
- Additional AArch64 UEFI assembly and representative legacy releases:
  `/tmp/fstart-common-aarch64-uefi.log`, `/tmp/fstart-common-legacy-{i945,pineview,riscv64,armv7}.log`.

The synthetic selection tests intentionally shuffle non-family unit names,
use different per-unit targets/entries/features, select only needed producers,
reject missing artifacts and prove changed producer bytes/configuration change
consumer output identity. Real LLD tests retain the AArch64 copy-extent and
fixed-overflow regressions while consuming the typed QEMU plan, not a deleted
Cargo recipe.

No hardware boot, stack high-water measurement or full board-matrix claim.
X61 generated images still contain **erased non-BIOS IFD/GbE/ME ranges** and are
not factory/full-chip backups. Do not blindly flash the entire generated image.

## Final board migration and legacy-route removal

All ten remaining boards now use the shared Rust platform-plan path, so the
"ten legacy packages remain" scope stated above is closed: `foxconn-d41s`
(Pineview), `qemu-q35`, `qemu-sbsa`, `qemu-sifive-u`, `sifive-unmatched`, and
the four sunxi boards (`bananapi-m1`, `orangepi-r1`, `orangepi-pc2`,
`licheerv-dock`). Board crates carry typed facts only; platform `host.rs`
resolvers own all build geometry. The `D41S remains legacy` note above is
superseded.

The legacy route is deleted, not deprecated:

- `tools/fbuild/src/main.rs`: the `cargo run fstart-host` fallback is gone;
  every board dispatches through `run_metadata` and must declare a `rust`
  build-profile.
- `tools/fbuild/src/board_tool.rs`: `BoardCallbacks`, `board_host_tool!`,
  the `Source::Legacy` path, and the payload-override/flash-normalize/
  manifest-validate helpers are gone; every command resolves a platform plan.
- `tools/fbuild/src/build_board.rs`: the `build_plan::plan` +
  `build_one_stage` executor and its SMM-artifact builder are gone; the plan
  executor owns stage and SMM compilation.
- `tools/fbuild/src/build_plan.rs`: the legacy BoardConfig feature/stage
  planner and its tests are gone; only `ParsedBoard` remains.
- `tools/fbuild/src/board_manifest.rs`: the top-level `features` recipe
  field is gone (deny-unknown-fields rejects it); selected-variant
  `variant_features` (e.g. the D945 postcar-debug variant) stay.
- `crates/stage/src/lib.rs`: the `StageBoard` trait and the
  `stage_bin!(Board)` arm are gone; only `stage_bin!(program: ...)` with a
  platform `StageProgram` remains. The `StageBoard` re-export in
  `platform-intel` is removed with it.

q35 fixes that fell out of the migration: `fbuild run` threads the linked
stage ELF from the plan build result into the x86 pflash (no more guessed
`fstart-stage.bin` path, which produced a flash without reset vector); the
pflash step patches the finalized FFS trust policy over the overlaid ELF's
placeholder trust block; q35 mounts the packed locator-bounded image instead
of the whole firmware window. q35 halt and UEFI now boot in QEMU; both failed
before (no serial output on the migrated tree, `mount_boot_media` failure on
the pre-migration baseline).

Known remaining issues, all reproduced identical on the pre-migration
baseline and therefore out of migration scope: `qemu-sbsa` halt (RAM
DTB discovery), `qemu-sifive-u` linux (`boot_integrity`), and the q35
uefi-disk guest-kernel memory-map failure (firmware boots and launches the
disk bootloader). `fstart-arch` clippy lints under the current toolchain
(e.g. `mp.rs` interior-mutable consts) predate this work.
