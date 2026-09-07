# Architecture migration baseline and progress

<!-- markdownlint-disable MD013 -->

This tracks the [architecture migration](architecture.md), starting from firmware
revision `38586a3e`. Historical sizes/budgets below are evidence, not build inputs
or measurements of stack high-water usage.

## Implemented scopes

### Discovery

All 14 live board manifests declare `schema = 1`. Typed TOML discovery scans the
excluded board inventory without compiling it, rejects unknown fstart keys and
malformed types, and validates one namespace for base/variant IDs. IDs must be
nonempty ASCII letters/digits/hyphens/underscores because they become artifact
path components. Other Cargo metadata namespaces remain unrestricted.

Legacy boards retain their existing metadata meanings and host path until their
own cutover. No fallback to directory-derived identity or unversioned metadata
exists. A legacy manifest cannot silently carry unused layout overrides.

### Layout wire

- `core::layout` is a borrowed, allocation-free view of little-endian bytes,
  with a versioned 16-byte header and at most 32 physical range records. It rejects
  malformed/truncated/trailing bytes, unknown roles, reserved bits, duplicate
  singleton roles, empty ranges and overflow.
- Image, writable footprint, stack, heap, mapped flash, firmware partition,
  payload, payload firmware and device tree have singleton roles. Additional
  reserved regions may repeat. These describe capacities, not file offsets.
- image-build encodes the immutable projection and validates it through the same
  decoder. Independent golden bytes check agreement.
- `fstart_stage::layout::current()` encapsulates linker-symbol access and validates
  bounds before forming a slice. Missing symbols fail linking, never fall back to
  board constants. `BYTE` emission produces loaded, read-only storage.

### QEMU RISC-V board integration

`qemu-riscv64` now uses metadata → `ResolvedBuild` → linker/descriptor/assembler.
Its board host executable, host config builder, Rust flash/placement constants
and shared-feature forwarding were removed. Its metadata selects a profile:

```toml
[package.metadata.fstart]
schema = 1
board = "qemu-riscv64"
platform = "riscv64"
stage-bin = "fstart-stage"
build-profile = { dependency = "fstart-platform-qemu", name = "riscv64-xip" }
```

The profile lives in `crates/platform-qemu/Cargo.toml`. Cargo metadata resolves
that actual direct dependency, including renamed dependency keys. The resolver
validates declared direct-dependency feature references and selects one supported
payload (`halt`, `linux`, `uefi`). Target, entry, environment, feature availability,
security inputs and all geometry come from the profile, not address heuristics.

Permitted board overrides under `[package.metadata.fstart.layout]` currently are
`image-capacity`, `writable-capacity`, `stack`, `heap`, `firmware-offset` and
`firmware-capacity`. Unknown overrides fail. Variant geometry overrides and other
entry/target combinations are not implemented yet.

Fixed reservations are allocated before compilation. The writable footprint is
split into data/BSS capacity followed by a fixed heap and stack. Section growth
cannot move either subrange. Linker regions/assertions and post-link PT_LOAD,
descriptor and symbol checks enforce the resolved capacities. The existing
assembler consumes a **derived** `BoardConfig` adapter; this is not a second
board-authored configuration or a compiler-selection input.

The board binary supplies `QemuRiscv64Program<Board>` to the program entry macro.
The platform-owned adapter legally implements `StageProgram`; board hooks and
hardware policy remain ordinary Rust. Payload availability is additive; the
explicit selection cfg chooses the launcher. The runtime reads the descriptor
for flash, FFS, heap/stack and payload placement. DTB-discovered RAM validates
reservations before installing the existing load policy, and supplies payload
RAM size rather than restating a fixed host memory map.

Compiled artifacts are isolated by board/profile and a digest of the resolved
selection/geometry, linker projection and base compiler flags. Each directory contains `resolved-build.json`, `rustflags.txt`,
`link.ld`, Cargo outputs, `stage.bin` and assembled images. Changing geometry
changes the `-T` path, forcing a relink. Linker-generator-only changes also
invalidate that path. Cargo JSON messages identify the actual
executable; no shared guessed stage binary is used on this path.

The commands below share the same resolver and compiler selection:

```sh
cargo run --locked -p fbuild -- explain qemu-riscv64 --payload halt
cargo run --locked -p fbuild -- check qemu-riscv64 --release --payload linux
cargo run --locked -p fbuild -- build --board qemu-riscv64 --release --payload halt
cargo run --locked -p fbuild -- run --board qemu-riscv64 --release --payload uefi \
  --firmware boot-assets/payloads/fw_dynamic.bin
```

`explain`, `check` and `ide` currently require a migrated board. Compilation still
uses the existing selected workspace, with a refreshed disposable lock followed
by `--locked` stage compilation. The editor uses a Cargo-derived JSON graph with
canonical source paths instead; see [editor usage and acceptance](ide.md).
Unrecorded `FSTART_EXTRA_RUSTFLAGS` is rejected on this path. Neither the editor
view nor its optional all-board lock audit changes source workspace ownership.

## Budgets and observed failures

Historical source budgets at the starting revision:

| Board/stage | Placement | Stack | Heap |
| --- | --- | --- | --- |
| qemu-riscv64 monolithic | Flash `0x20000000`; writable data `0x81000000` | 1 MiB | 256 KiB |
| lenovo-x61 bootblock | Top-of-ROM sentinel `0xffffffff` | 8 KiB | None |
| lenovo-x61 postcar | `0x01000000` | 8 KiB | None |
| lenovo-x61 ramstage | `0x04000000` | 4 MiB | 2 MiB |

RISC-V virt still uses 32 MiB flash: a 16 MiB stage-image capacity and 16 MiB FFS
partition. Its new fixed writable footprint is **4 MiB** at `0x81000000`, including
256 KiB heap and 1 MiB stack. The first 2 MiB budget failed CrabEFI's release link:
initialized data plus BSS needed about 1.1 MiB, but only 768 KiB remained after
heap/stack allocation. The profile budget was explicitly raised; the linker did
not silently relocate anything. These values are maintained only in the profile.

CrabEFI boot also exposed an OpenSBI DTB-growth issue: QEMU's source was registered
as 5,044 bytes, but OpenSBI returned a 6,100-byte blob. The bounded reader correctly
rejected it. The RISC-V UEFI handoff now copies the source into the registered
64 KiB destination before invoking OpenSBI and requires fixup slack. It does not
relax the original source bound. Root authentication, executable verification,
image-directory formats and SMM packaging were not redesigned.

## Validation evidence

Before migration, a cached QEMU RISC-V halt release build reported 1.52 seconds;
ELF size was 127,728 bytes and flat stage size 75,056 bytes. This was not a clean
build timing baseline. Existing unmatched profile-override and unused SiFive-U
config warnings remain leads for the compilation-closure cleanup.

Current checks:

- `cargo test --locked -p fbuild -p fstart-core -p fstart-image-build --lib`:
  41 passed. Includes deterministic resolution, geometry projection/override,
  invalid ranges/budgets/selections, direct feature-reference validation, editor
  unit projection and normalized build-lock graph comparison.
- `cargo test --locked -p fstart-stage --lib --features ffs,fdt -- --test-threads=1`:
  13 passed. Includes a focused DTB-growth test preserving the source bound while
  accepting bounded growth in the dedicated destination.
- Real LLD tests retain descriptor bytes under `--gc-sections` in little-endian
  RISC-V and big-endian AArch64 ELF files, and through PT_LOAD flat extraction.
  These fixtures are not AArch64 boot or relocation evidence.
- QEMU RISC-V release matrix: halt, Linux userspace (`FSTART_CI_BOOT_SUCCESS`) and
  CrabEFI (`Boot manager finished`) all passed. Halt also reached PCI enumeration.
- Unmigrated QEMU SiFive-U halt release boot passed, checking the legacy path.
- `fbuild check` succeeded for Linux. A target check with both Linux and CrabEFI
  available and Linux selected passed; missing/multiple selections and a disabled
  selected backend each failed with the expected compile-time diagnostic.

A metadata-only experiment doubled heap to `0x80000` and moved FFS to flash
offset `0x01200000` with capacity `0x00e00000`. It changed the linker digest/path,
flat descriptor and assembled flash descriptor, and boot logged the new heap/FFS
sizes and reached the payload boundary. Both base and override boots passed; the
board manifest was restored afterward. Host unit tests retain projection and
relink-key coverage. Development-run logs are under `target/qemu-boot-tests/`;
geometry/selection experiment logs are `/tmp/fstart-geometry-proof.log` and
`/tmp/fstart-selection-proof.log`.

The editor increment passed real-source navigation, proc-macro expansion,
compiler-diagnostic/error-recovery and payload/profile switching probes. An
additional 100 temporary inventory boards left its graph unchanged. The all-board
lock prototype (14 boards / 35 members) agreed with the selected QEMU compiler
graphs and external pins; it was not promoted. Full evidence and remaining editor
gates are in [editor usage and acceptance](ide.md).

No full board matrix, fresh Intel hardware validation, clean-build benchmark or
stack high-water measurement is claimed.
X61 cold boot remains hardware-validation outstanding. Emulator boots remain
explicitly development-integrity, not hardware-authenticated boot.

## Next gates

- Extend editor acceptance to cross-ISA/multistage switching, clean regeneration
  and macro/build-script edit workflows before workspace/lock cutover.
- Extend resolved entry/layout support to further QEMU targets, proving relocation
  where applicable; then cut over the Intel multistage boundary and reservations.
- Retain one authoritative path per migrated board. Remaining boards still use
  their old host path; do not remove it globally before their replacements work.
- Complete input-digest/reproducibility reporting, canonical build-lock ownership
  and actual module/dependency gating. No per-board lock removal is claimed here.
