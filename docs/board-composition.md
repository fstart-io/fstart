# Board and platform composition

<!-- markdownlint-disable MD013 -->

The authoring boundary is ordinary Rust composition, not a generated hardware
schema. Boards describe hardware and select a platform; platforms own fixed
flows and build policy; drivers implement hardware mechanisms.

## Intel: bind identity once

An unconditional board source module implements the host-clean facts contract:

```rust
impl fstart_platform_intel::IntelBoardFacts for Board {
    type Platform = fstart_platform_intel::i945::I945Ich7<
        fstart_platform_intel::legacy_cpu::Socket441,
    >;
    const CONFIG: &'static fstart_platform_intel::i945::I945Ich7Platform =
        &D945GCLF_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE);
}
```

The config is a board `static` built with the platform's const builder. The
associated hardware marker supplies host policy and runtime driver associations.
CPU population comes from that same config. There is no second chipset enum or
host CPU count for the board to keep synchronized.

For legacy discrete-northbridge systems, CPU selection is independent of the
chipset: `I945Ich7<Socket441>` selects Diamondville Atom, whereas
`I945Ich7<SocketM>` selects the mobile Core/Core 2 package profile. D41S uses
`PineviewIch7<Fcbga559>`. These follow coreboot's board-to-socket-to-model policy,
not a rule that i945 implies Atom. Profiles own microcode coverage, CAR geometry,
and the model driver; runtime microcode matching still checks CPUID/platform ID.
`SocketM` includes the 6EX/6FX microcode families, but runtime initialization
currently supports the Core 2 subset, not Yonah; unsupported CPUIDs are rejected.
No Penryn or NetBurst runtime support is implied by this change.

Socket M defaults to coreboot's 32-KiB CAR window. X61 explicitly retains its
existing layout with `Gm965Ich8<SocketM<0xfef0_0000, 0x80000>>`; this work does
not shrink its boot-time memory budget. Board hooks refer to the board's
`Hardware` alias so they do not repeat the package selection. Newer integrated
platforms can keep CPU policy platform-owned; they do not need socket profiles.

Flash capacity, partitioning and attached-device assets remain board facts.
Stage budgets, load addresses, target policy and microcode defaults do not.

The runtime `IntelBoard` implementation supplies:

- A console type, config and diagnostic node name.
- `EarlyHooks` for CAR and `MainstageHooks` for RAM, each with fresh `Default`
  state. Only the selected stage's association is compiled.
- Board SMBIOS identity in mainstage.

There is no payload association or trivial hook constructor to forward. Entry
is a one-line call to `fstart_platform_intel::stage_bin!(board_crate::Board)`.
QEMU, Sunxi and the current FU740 flow also invoke their selected launcher
inside the flow rather than requiring board payload forwarding.

## Stage contracts, not lifecycle replay

The handwritten Intel flow calls these distinct contracts:

| Stage | Hook | Prerequisites |
| --- | --- | --- |
| CAR | `before_console` | Chipset pre-console setup and LPC decode; no console, heap or allocated PCI resources |
| CAR | `before_memory` | Console and chipset early setup; DRAM not trained yet |
| CAR | `after_memory` | DRAM training/recovery and early post-DRAM chipset setup complete |
| CAR | `before_handoff` | Successor authenticated/loaded and postcar stash published |
| RAM | `before_console` | DRAM/heap available and LPC decode established; memory map not reconstructed yet |
| RAM | `after_devices` | Memory map reconstructed/reservations applied, PCI resources assigned, chipset device setup complete; MP/SMM setup still follows |
| RAM | `before_handoff` | Device/display setup and table emission complete; chipset finalization and AP parking still follow |

These hooks execute on cold, warm and S3 paths. Mainstage context exposes the
S3 flag and the flow-owned memory map. CAR state does not survive into RAM;
postcar does not instantiate board hooks. A board can use different hook types
for each stage, even though the existing Intel boards currently need only one
small type with two explicit trait implementations.

Sharing a concrete board operation at two seams is fine. Calling the *early
lifecycle contract* again to reconstruct mainstage state is not. For example,
X61 explicitly establishes dock console routing in both stages, but EC/PMH7
bring-up belongs only to mainstage's `after_devices`, independently of ACPI
emission. Its device-specific dock/DLPC code remains board-owned.

D945GCLF instead uses a reusable SMSC operation: the driver owns PME LDN
selection, disable/base/enable programming and config-mode entry/exit. The
board supplies `0x680` and orders PME before COM/KBC initialization. A focused
register-order test verifies that mechanism; it does not prove electrical
behavior or hardware boot.

## Availability features, not board selection features

Each Intel board enables its chipset pair on its direct platform dependency:

```toml
fstart-platform-intel = { path = "../../../crates/platform-intel", features = ["i945-ich7"] }
```

The platform feature enables actual driver features and gates its pairing
module. Driver features gate the actual chipset modules, including raminit.
i945 and Pineview still share the same ICH7 implementation. Shared IP mechanisms
remain shared; gating does not copy algorithms or add a crate per board.

Availability is additive: tests and tools may enable several chipset pairs.
Board selection is package selection, not an exactly-one chipset feature rule.
Stage/entry/payload selections remain explicit compiler cfgs supplied by the
resolved platform plan. The stage launcher and Sunxi's block-device launchers
use `fstart_payload`, not backend feature precedence. Sunxi mainstage bundles
can therefore include Linux support while a halt selection still halts. Missing
selected backends and conflicting selections are rejected by the stage crate.
A focused type-identity test exercises halt/Linux/UEFI with both backends enabled.

## Build multiple boards as independent Cargo configurations

```sh
cargo fbuild build -b lenovo-x61 --release --payload halt
cargo fbuild build -b intel-d945gclf --release --payload uefi
cargo fbuild build -b foxconn-d41s --release --payload halt
cargo fbuild ide lenovo-x61 --release --payload uefi --stage ramstage
```

`fbuild` resolves a board through its real Cargo dependencies and asks its
platform for concrete compiler units. Cargo still handles dependencies, macros,
build scripts and linking. `fbuild` handles stage producer dependencies, layout,
ELF validation and image assembly. Adding another board on an existing platform
must not require a board-name match in the tool or a copied stage flow.

Build/check/IDE use the same resolved selection. A workspace may inventory all
boards without building all architectures or unifying all board/stage features
into a single firmware compilation. Artifact paths remain selection-specific.
This work does not change workspace or lock ownership; the separate cutover
and editor acceptance gates in [architecture.md](architecture.md) still apply.

## Validation scope

Reproduce the additive-backend test on the host:

```sh
for payload in halt linux crabefi; do
    RUSTFLAGS="--cfg fstart_payload=\"$payload\"" cargo test --locked \
        -p fstart-stage --lib --features x86_64,linux,crabefi-basic \
        selection_is_independent_of_available_backends
done
```

The Intel host comparison suite enables all three hardware pairs:

```sh
FSTART_SMBIOS_DATE=09/30/2026 cargo test --locked -p fstart-platform-intel \
    --features host,gm965-ich8,i945-ich7,pineview-ich7 --lib
python ci/ide-tests.py --intel --audit-lock
```

Release linking, image assembly, emulator boot and hardware boot are distinct
claims. The changes here preserve fixed flow ordering; they do not change
register sequences in chipset raminit, X61 dock handling, SMM installation or
image authentication. Hardware boot and stack high-water evidence remain
separate outstanding work.
