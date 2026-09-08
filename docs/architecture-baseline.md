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
entry/target combinations beyond the three QEMU virt profiles are not implemented yet.

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

### ARMv7 and cross-ISA increment

`qemu-armv7` now selects `armv7-xip` from the same platform manifest and uses
`QemuArmv7Program<Board>`. Its host binary, authored Rust geometry and feature
forwarding are removed. Hardware policy and the console/PCI/init sequence remain
handwritten. ARMv7 boots Linux directly: no SBI/TF-A firmware input is fabricated.

The profile retains two 64 MiB flash banks: stage in bank 0, FFS in bank 1. Its
fixed writable footprint is 1 MiB at `0x40200000`, including 256 KiB each for heap
and stack; the remaining 512 KiB is data/BSS capacity. The kernel reservation is
64 MiB at `0x41000000`; the destination DTB remains 64 KiB at `0x40f00000`.
Runtime firmware/payload placement comes from the descriptor; RAM size comes from
the DTB-discovered load policy, not a second static board memory map.

The linker selects ARM anchors/architecture and the post-link validator handles
ELF32 and ELF64 while checking target architecture, endianness and class.
ARMv7 reservations, including their ends, must fit 32-bit addresses. Real LLD
fixtures exercise writable-anchor retention and descriptor validation in both
RISC-V ELF64 and ARM ELF32. ARM halt/PCI and Linux userspace release boots passed
before and after migration. RISC-V ↔ ARMv7 editor switches passed in one LSP
session, with both ARM selections also passing the all-board lock prototype audit.
Logs: `/tmp/fstart-armv7-resolved-boots.log`, `/tmp/fstart-cross-isa-tests.log` and
`/tmp/fstart-cross-isa-ide.log`. The focused fbuild suite now has 23 passing tests.

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

### AArch64 baseline repair before layout migration

The first AArch64 baseline reached PCI setup but both Linux and UEFI failed
loading BL31. QEMU describes its 16 MiB secure RAM at `0x0e000000` as
`secram@e000000`, with `device_type = "memory"`, `status = "disabled"` and
`secure-status = "okay"`. The old discovery loop only considered `memory@*`
names, so BL31's destination `0x0e090000` was correctly denied by the installed
load policy because the hardware RAM window had never been registered.

Discovery now uses device type and availability properties. Secure-only RAM is
admitted only during initial setup when the architecture entry recorded its
existing EL3 → Secure EL1 path. Unknown incoming EL1/EL2 states conservatively
receive no secure access. This boot receipt does not describe security state
after TF-A handoff. The EL transitions, MMU setup and authenticated loader remain
unchanged; the fix does not permit arbitrary firmware destinations.

AArch64 halt/Linux/CrabEFI and ARMv7 halt/Linux release boots pass after the fix.
The same AArch64 image booted non-secure, both via EL2 and directly at EL1, rejects
BL31 loading and registers no secure RAM. A focused host test covers the status
precedence and denial cases without linking the firmware allocator. Evidence:
`/tmp/fstart-secure-ram-boots.log`, `/tmp/fstart-secure-ram-tests.log` and
`/tmp/fstart-aarch64-nonsecure-{on,off}.log`. Those tests used the legacy layout
before the separate relocation migration below.

### AArch64 resolved relocation

`qemu-aarch64` now selects `aarch64-relocate` and supplies
`QemuAarch64Program<Board>`. Its old host binary, Rust geometry builders and
feature-forwarding layer are removed. The two flash banks remain unchanged.
The profile separately reserves:

- Image storage: 64 MiB at flash address zero.
- Initial RAM execution copy: 4 MiB at `0x40400000`, including data initializers.
- Writable RAM: 8 MiB at `0x40800000`, with 4 MiB data/BSS/page-table capacity,
  a fixed 1 MiB heap and 3 MiB stack ending at `0x41000000`.
- Linux: 64 MiB at `0x41000000`; BL31: 2 MiB at `0x0e090000` in discovered
  secure RAM; destination DTB: 64 KiB at `0x40100000`.

These are explicit capacity budgets, not measurements or section-growth-driven
placements. The descriptor's `Execution` role describes the initial RAM copy;
`Image` continues to describe storage. Runtime setup validates and excludes the
whole execution/writable reservations from subsequent authenticated loads. UEFI
receives a copy of the source DTB in the registered destination workspace.

The existing reset copier still copies `_binary_end - _start`. The linker now
provides distinct physical flash and virtual RAM addresses, computes the exact
stored copy extent including initialized data, and points `_data_load` into the
copied RAM image. This keeps common entry and running-stage bounds consistent.
Explicit placement of retention/unwind sections and inclusion of UEFI vectors
in text prevent LLD orphans from acquiring incorrect physical addresses.
Post-link validation checks the linear execution mapping, copy extent, target,
descriptor and fixed boundaries; real LLD fixtures test vector alignment, data
source addresses, flat descriptor retention and capacity/extent rejection.

Release halt/PCI, Linux userspace and CrabEFI boots all passed on this path
(`/tmp/fstart-aarch64-resolved-boots.log`). This is emulator relocation evidence,
not Intel multistage or hardware validation. Workspace/lock ownership is unchanged.

A metadata-only experiment moved execution to `0x40200000`; ELF validation and
halt boot passed at that address, and the profile was restored. Final validation
passed 44 host tests, editor/lock checks across all three ISAs (including 100
unrelated inventory boards), and all RISC-V/ARMv7 release boot regressions.
Evidence: `/tmp/fstart-aarch64-move-proof.log`, `/tmp/fstart-aarch64-final-tests.log`,
`/tmp/fstart-aarch64-final-ide.log` and `/tmp/fstart-aarch64-final-regressions.log`.

## Validation evidence

Before migration, a cached QEMU RISC-V halt release build reported 1.52 seconds;
ELF size was 127,728 bytes and flat stage size 75,056 bytes. This was not a clean
build timing baseline. Existing unmatched profile-override and unused SiFive-U
config warnings remain leads for the compilation-closure cleanup.

Current checks:

- `cargo test --locked -p fbuild -p fstart-core -p fstart-image-build --lib`:
  43 passed (plus the platform memory-visibility test: 44 total). Includes deterministic resolution, geometry projection/override,
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

## Lenovo X61 metadata cutover

X61 now selects `fstart-platform-intel:gm965-car` metadata. One Intel aggregate
resolves the three fixed compiler roles, security, IFD, microcode and SMM policy.
SMM is built first from the board binding; only ramstage receives its image,
whose contents participate in the compiler artifact identity. The old X61 host
binary, BoardConfig builder, flash helpers and generic Cargo feature relays are
removed. GM965 consumes the linked descriptor; i945/Pineview retain legacy bounds.

Full BIOS identity remains `0xffe80000/0x180000`, including the bootblock tail at
`0xfffc0000/0x40000`. The filesystem therefore has `0x140000` bytes before the
fixed bootblock. CAR/postcar have no heap. RAM stages keep distinct image and
writable reservations; their initialized flat files include any intervening gap.

| Release measurement | Halt | UEFI |
|---|---:|---:|
| Bootblock flat extent | 262,144 | 262,144 |
| Postcar initialized flat bytes | 20,868 | 20,868 |
| Ramstage initialized flat bytes | 4,194,312 | 5,334,792 |
| BIOS FFS bytes | 220,770 | 893,658 |
| Full IFD flash bytes | 4,194,304 | 4,194,304 |

Both images include seven microcode inputs totaling 86,016 bytes and an
11,856-byte SMM image with two entries. The larger flat files are fixed placement
(including padding), not measurements of live code or runtime stack use.

The allocation-free runtime adapter checks complete successor reservations
against the profile envelope and trained/inherited RAM cap. Signed role, exact
load address, zero entry offset and bootstrap output-plus-input footprint remain
mandatory. Complete current Image/Writable and persistent exclusions accompany
E820-based load policy. Authentication, stash ABI, CAR teardown and hardware
phase ordering are preserved; the descriptor is not authentication.

Evidence from this cutover:

- Final release halt/UEFI links and full assembly:
  `/tmp/fstart-x61-cutover-final-{halt,uefi}.log`.
- Metadata-only postcar move to `0x01200000` links and assembles; shrinking the
  bootblock to 64 KiB fails at the linker. Both experiments were restored:
  `/tmp/fstart-intel-metadata-{move,overflow}.log` and
  `/tmp/fstart-intel-metadata-proof.log`.
- 30 fbuild, 11 core, 8 image-build and one Intel layout unit test pass;
  UEFI check and all three stage-specific editor generations pass. The ramstage
  all-board lock prototype agrees. QEMU RISC-V, ARMv7 XIP and AArch64 relocation
  release halt links pass: `/tmp/fstart-intel-editor-regression-gates.log`.
- Original-source editor graphs, isolated cfg/features and SMM producer inputs
  were inspected: `/tmp/fstart-intel-editor-inspection.log`. Both ramstage flat
  images contain the exact generated SMM bytes:
  `/tmp/fstart-intel-smm-embedding.log`.
- Legacy i945/ICH7 (`intel-d945gclf`) and Pineview/ICH7 (`foxconn-d41s`) release
  halt links pass: `/tmp/fstart-intel-legacy-pairing.log`.

No new emulator boots, live Intel rust-analyzer switching, full board matrix,
stack high-water measurements or X61 hardware boots are claimed. Source workspace
and lock ownership are unchanged; board locks were not edited.

## Next gates

- Extend editor acceptance to multistage switching, clean regeneration
  and macro/build-script edit workflows before workspace/lock cutover.
- Extend descriptor-backed Intel geometry to the remaining families and obtain
  X61 hardware evidence separately from successful links and assembly.
- Retain one authoritative path per migrated board. Remaining boards still use
  their old host path; do not remove it globally before their replacements work.
- Complete input-digest/reproducibility reporting, canonical build-lock ownership
  and actual module/dependency gating. No per-board lock removal is claimed here.
