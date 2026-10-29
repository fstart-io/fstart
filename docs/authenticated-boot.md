# Authenticated boot

Reference for the FFS boot root, trust policy, bootstrap loading and packaging
with `fbuild`. Current boards are **development-integrity** builds.

## Status

| Property | Current behavior |
| --- | --- |
| Root signature | Ed25519 over the first 448 bytes of the boot root. The builder rejects ECDSA roots. |
| Keys | One key, key ID 0, embedded in the trust block: the `--signing-key` given to `fbuild assemble`, or an ephemeral key generated for that assembly. |
| Image family | First 16 bytes of SHA-256 of the board name. |
| Rollback | Minimum security version is 0 and the root's `security_version` is 0. No persistent floor exists. |
| Trust anchor | The linked trust block (`FSTRUST1`) and the bootblock/SPL code. Nothing verifies them. |
| Sunxi | No protected trust anchor (`crates/platform-sunxi/src/boot.rs`). The `FSTPIN01` pin detects corruption of the next stage, not image replacement. |

Not provided: hardware secure boot, persistent rollback, flash write protection,
DMA isolation, RAM authentication, or an authenticated update flow. Intel logs
`boot trust: development-integrity`; mainstage logs
`boot profile: development integrity; rollback not enforced`.

## Components

| Path | Contents |
| --- | --- |
| `crates/core/src/ffs/locator.rs` | `LocatorBlock`: media offsets only. |
| `crates/core/src/ffs/trust.rs` | `TrustBlock`: family, minimum security version, up to 4 keys. |
| `crates/ffs/src/root.rs` | Boot root (512 bytes), bootstrap descriptors, `authenticate_root`. |
| `crates/ffs/src/manifest.rs` | Directory revision 2 encode/parse. |
| `crates/ffs/src/builder.rs`, `builder/trust.rs` | Host-only image builder (`std` feature). |
| `crates/crypto/src/` | SHA-256/SHA3-256 (`digest.rs`), Ed25519 and ECDSA P-256 (`verify.rs`). |
| `crates/stage/src/root.rs` | `read_boot_root`, `authenticate_boot_root`. |
| `crates/stage/src/boot.rs` | `MemoryPolicy`, `load_bootstrap`, `VerifiedExecutable`, `running_stage_windows`. |
| `crates/stage/src/directory.rs` | Mainstage directory cache and load policy. |
| `crates/stage/src/loaded.rs` | Load ledger and FDT workspace guard. |
| `crates/stage/src/ffs_helpers.rs` | File and data-asset loaders. |
| `crates/stage/src/fdt_workspace.rs` | FDT destination and bounded FDT preparation. |
| `crates/stage/src/anchor.rs`, `next_stage.rs` | Linked trust block, locator, SRAM pin. |
| `crates/core/src/services/ffs_context.rs` | Verified data-asset service. |
| `crates/core/src/security.rs` | `SecurityConfig`: board algorithm policy (signature algorithm, required digests). No key material. |
| `crates/image-build/src/assemble.rs` | Keys, root policy, packaging, Sunxi pin patching. |
| `crates/image-build/src/inspect.rs` | `fbuild inspect` output. |

## On-media format

All fields are little-endian. Reserved bytes must be zero.

### Image layout

```
[ regions: files ]
[ pad to 8 bytes ]
[ trust block, 320 bytes ]
[ directory, revision 2 ]
[ boot root, 512 bytes ]
```

The 40-byte locator sits in the initial stage at the `FSTART_ANCHOR` placeholder
(`.fstart.anchor`). The builder patches it. Compressed files may not contain one.

### Locator (`FSTART01`, version 7, 40 bytes)

| Offset | Field | Meaning |
| --- | --- | --- |
| 0 | `magic` | `FSTART01` |
| 8 | `version` | `u32` = 7; version 6 is rejected |
| 12 | `manifest_offset` | Offset of the boot root |
| 16 | `manifest_size` | 512 |
| 20 | `total_image_size` | Image length |
| 24 | `anchor_offset` | Offset of the locator |
| 28 | `microcode_offset` | Image offset of `cpu_microcode_blob.bin`, or 0 |
| 32 | `microcode_size` | Size, or 0 |
| 36 | `image_offset` | 0 for the initial image |

The locator is transport only. It cannot supply keys, family or version floor.

### Trust block (`FSTRUST1`, version 1, 320 bytes)

| Offset | Field |
| --- | --- |
| 0 | `magic` |
| 8 | `version` = 1 |
| 12 | `encoded_size` = 320 |
| 16 | `minimum_security_version` (`u64`) |
| 24 | `key_count` (≤ 4) |
| 28 | `reserved` = 0 |
| 32 | `image_family` (16 bytes) |
| 48 | `keys[4]`, 68 bytes each |

The builder replaces the placeholder in every file that contains one, before compression.

### Boot root (`FSTARTBR`, revision 1, 512 bytes)

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | `FSTARTBR` |
| 8 | 2 | Version = 1 |
| 10 | 2 | Header length = 64 |
| 12 | 4 | Total length = 512 |
| 16 | 8 | `security_version` |
| 24 | 16 | `image_family` |
| 40 | 4 | `key_id` |
| 44 | 20 | Zero |
| 64 | 160 | Bootstrap descriptor 0 |
| 224 | 160 | Bootstrap descriptor 1 |
| 384 | 64 | Directory reference |
| 448 | 64 | Ed25519 signature over bytes 0..448 |

Directory reference: `offset` (u64), `size` (u64), SHA-256 (32), `revision`
(u32 = 2), 12 zero bytes. An all-zero descriptor slot is absent; slot 1 cannot be
set while slot 0 is absent.

### Bootstrap descriptor (160 bytes)

| Offset | Field |
| --- | --- |
| 0 | `role` (u16): 1 = Postcar, 2 = Mainstage |
| 2 | `compression` (u8): 0 = none, 1 = LZ4 block |
| 3 | Zero (5 bytes) |
| 8 | `offset` of stored bytes in the image |
| 16 | `stored_size` |
| 24 | `loaded_size`: initialized bytes only |
| 32 | `load_addr` |
| 40 | `entry_offset` (`< loaded_size`) |
| 48 | `scratch_size` |
| 56 | Stored SHA-256 |
| 88 | Loaded SHA-256 |
| 120 | Zero (40 bytes) |

`validate()` requires: nonzero stored and loaded sizes; stored range inside the
image; `load_addr + loaded_size` without overflow. LZ4 needs
`scratch_size ≥ stored_size + loaded_size`. Uncompressed needs
`scratch_size == 0`, `stored_size == loaded_size`, and equal digests.

### Directory (revision 2, `FSMZ`)

A 28-byte header (magic, revision, region/entry/segment counts, string-table
offset and size), then region records (24 bytes), entry records (88 bytes),
segment records (100 bytes), and NUL-terminated UTF-8 strings. Trailing bytes are
rejected.

- File types: stage 1, board-config 2, payload 3, FDT 4, data 5, raw 6,
  firmware 7, FIT 8, microcode 9, initramfs 10.
- Entry digest flags must be 1 (SHA-256). The SHA3 field is zero.
- Each stored segment carries stored and initialized SHA-256 values, a memory
  length, and an in-place workspace size.
- Names are unique per region; ambiguous lookups fail.
- Mainstage limits the directory to 64 KiB (`MAX_DIRECTORY_SIZE`).

The directory is trusted only through the root's `DirectoryRef` digest, or
through a verified predecessor's handoff.

## Authentication

`fstart_ffs::root::authenticate_root`, called by `fstart_stage::root`:

1. Parse the root. Reject bad magic, length, version or reserved bytes.
2. `image_family` must equal the trust block's family.
3. `security_version` must be ≥ the trust block's minimum (0 today).
4. Directory size ≤ 64 KiB and inside the image.
5. Each present descriptor passes `validate` against the image size.
6. `key_id` must fit in `u8` and match exactly one trust-block key.
7. The Ed25519 signature verifies over bytes 0..448 (`verify_strict`).

Family, keys and minimum version come from the linked trust block. The locator
supplies only the root offset and image size, checked against platform media
capacity. Parsing checks structure; authentication checks policy. Neither grants
write or execute authority.

## Packaging: `cargo fbuild assemble`

Source: `crates/image-build/src/assemble.rs`. `tools/fbuild` is the CLI wrapper.

**Keys.** There are no default or per-board keys. Pass the signing key with
`--signing-key <PATH>` (on `assemble`, `run`, `flash`): an Ed25519
PKCS#8 private key, PEM or DER.

```sh
openssl genpkey -algorithm ed25519 -out signing.pem
cargo fbuild assemble -b qemu-riscv64 --signing-key signing.pem
```

Without `--signing-key`, the image is signed with an ephemeral key generated in
memory and discarded after the build, so nothing can be re-signed for that
image later. Either way the public key is printed and written into the trust
block. Only `SignatureAlgorithm::Ed25519` is implemented; other algorithms are
rejected at assembly.

**Root policy.** Family is SHA-256 of the board name (first 16 bytes). Security
version is 0. Bootstrap bindings come from the resolved plan. Without a plan,
every stage after the first is bound: `postcar` → Postcar, others → Mainstage.

**Builder rules** (`crates/ffs/src/builder.rs`):

- At most 2 bootstrap stages, with unique names and roles.
- Each bootstrap file is one flat executable segment with
  `initialized_size == loaded_size` (no BSS).
- Compressed files may not contain a locator.
- Duplicate key IDs are rejected. The signer must use the selected Ed25519 key;
  the builder verifies the signature itself.

**Sunxi** (`SocImageFormat::AllwinnerEgon`): the initial stage must contain one
160-byte `FSTPIN01` placeholder when more than one stage is present. It is
replaced by the mainstage descriptor. eGON fields are patched (next-stage offset
at 0x2c, size at 0x30, total size at 0x34), then `finalize_initial_stage`
re-hashes the directory and re-signs the root.

**Outputs:** `<board>.ffs` and `<board>.inputs.txt` (SHA-256 of each patched
input and of the trust block). The final path is printed; for resolved boards it
is under `target/fstart-build/<board>/<release|debug>/<digest>/image/`.

### CLI (`tools/fbuild/src/main.rs`)

`cargo fbuild` is an alias for `cargo run --package fbuild --`.

| Command | Use |
| --- | --- |
| `build --board <name> [--release] [--payload <p>] [--stage <unit>]` | Build stages. |
| `assemble --board <name> [--release] [--payload <p>] [--kernel P] [--firmware P] [--fit P] [--payload-file NAME=PATH]` | Build and package the image. |
| `inspect --image <path>` | Print anchor, trust family, boot root and directory. |
| `run --board <name> [--release] [--payload <p>]` | Assemble and run under QEMU. |

`--payload`: `uefi`, `uefi-ui`, `uefi-basic`, `linux`, `coreboot`, `halt`.
Board names are the `board` value in `boards/<vendor>/<board>/Cargo.toml`
(e.g. `lenovo-x61`, `intel-d945gclf`, `qemu-q35`, `bananapi-m1`).

```sh
cargo fbuild assemble --board lenovo-x61 --release
cargo fbuild assemble --board bananapi-m1 --release --payload halt
cargo fbuild inspect --image <path printed by assemble>
cargo fbuild run --board qemu-riscv64 --release --payload halt
```

## Loading (stage)

### Mainstage sequence

1. `root::authenticate_boot_root`: reads the trust block and locator, reads the
   512-byte root, authenticates it.
2. `directory::install_directory_context`: reads the directory once into heap
   memory, checks its SHA-256 on those exact bytes, then parses them. Later
   views reuse those bytes; the directory is never re-read from media.
3. `directory::set_load_policy`: platform windows. `grant_load_window` adds one
   (used for the coreboot legacy shadow).
4. Loads, using the policy and ledger.

Mainstage also installs `read_verified_asset` as the verified asset service.

### MemoryPolicy

```rust
pub struct MemoryPolicy<'a> {
    pub writable: &'a [MemoryWindow],
    pub reserved: &'a [MemoryWindow],
    pub entry_alignment: u64, // power of two
}
```

A destination must lie inside one writable window and outside every reserved
window; address 0 is rejected. The platform reserves everything live: the
running image and stack/heap/tables (`running_stage_windows()` gives the
linker-derived image and writable ranges, implemented for `target_os = "none"`),
the handoff, temporary arenas, and DMA buffers.

### Load paths

| Path | Entry point | Notes |
| --- | --- | --- |
| Descriptor | `boot::load_bootstrap` | No allocator. Intel postcar/mainstage, Sunxi SPL. |
| SRAM pin | `next_stage::pinned_bootstrap_descriptor` | Sunxi SPL. No signature check. |
| Directory file | `ffs_helpers::load_ffs_file_by_type`, `_by_name`, `load_ffs_file_entry_by_type` | Ledger-checked. The entry variant returns the verified entry. |
| Owned bytes | `ffs_helpers::find_ffs_file_data` | Uncompressed only. Heap copy, retained. |
| Data asset | `ffs_context::read_verified_asset` / `ffs_helpers::read_asset` | Heap copy. Ignores `load_addr`. |
| Microcode | `ffs_helpers::microcode_blob` | Authenticated 16-byte-aligned copy. |

Directory files with more than one segment fail before any write. A segment
with BSS is zero-filled after its verified load.

### load_bootstrap rules

- `entry_offset < loaded_size`; `entry_alignment` is a power of two and the
  entry is aligned.
- The full footprint is checked before the first write: `loaded_size`
  (uncompressed) or `loaded_size + stored_size` (LZ4).
- Short workspace → `Bounds`. LZ4 without the `lz4` feature → `Unsupported`.
  No streaming fallback exists.
- Uncompressed: read into the destination; check stored and loaded digests.
- LZ4: read the stored bytes into the disjoint scratch, check the stored digest
  on exactly those bytes, decompress into the output, then check the loaded
  digest. Output and scratch come from `split_at_mut`.
- A memory-mapped media slice must not overlap the destination.
- `VerifiedExecutable` is returned only after all digests pass.

### Ledger (`crates/stage/src/loaded.rs`)

- Each successful non-FDT load commits its full footprint (initialized bytes
  plus BSS). Up to 32 windows. Windows survive policy refreshes.
- A later load overlapping a committed window fails. Loads are serialized; a
  concurrent load fails with `MemoryPolicy`.
- FIT kernel+ramdisk plans use at most 2 windows.

### FDT workspace

- `fstart_stage::configure_fdt_workspace(source, destination)` registers one
  destination. Re-registering the same pair is accepted.
- No other load may overlap the workspace, and FDT loads must be non-executable.
- `fdt_prepare_platform` (feature `fdt`) checks the source header and the
  destination size against the used size plus a fixed 8 KiB patch allowance,
  copies, then patches.
- Patching never grants write access to verified code.

### Payload entries

Payload helpers launch the entry actually verified (`VerifiedExecutable::entry`).
A configured entry that does not match fails `checked_entry`, and the payload
halts instead of jumping to the configured address (`crates/stage/src/payload.rs`).

## Per-platform use

| Platform | Flow |
| --- | --- |
| Intel (`crates/platform-intel`) | Bootblock authenticates the root and loads Postcar. It writes the postcar stash at `0x2000` (`PCST`, version 3; `crates/arch/src/x86/boot/car_teardown.rs`) containing the Mainstage descriptor, directory reference, family, minimum version, locator and boot flags. Postcar loads and verifies ramstage before entry. Mainstage imports the directory and installs an E820-derived policy. |
| QEMU (`crates/platform-qemu`) | Mainstage policy from `running_stage_windows()` and platform RAM. |
| Sunxi (`crates/platform-sunxi`) | SPL loads only the pinned descriptor. DRAM must start at `0x4000_0000`, reserve the handoff, and use 4-byte entry alignment. Mainstage receives only the locator (`install_locator`), no keys or root. |

Under `fstart_stage_env = "ram"` or `"postcar"`, the locator arrives from the
predecessor's handoff through `anchor::install_locator`, once, before any media
service runs.

Driver assets: `crates/driver-intel/src/igd.rs` (`locate_vbt`) reads a configured
VBT through `read_verified_asset`. If that fails it returns `None` and does not
probe legacy memory. Board data files are packaged as `FileType::Data`.

## Stage features

| Feature | Enables |
| --- | --- |
| `bootstrap` | Descriptor loading and SHA-256 (implies `sha2-digest`). |
| `ffs` | Mainstage directory. |
| `ffs-signature` | Boot-root Ed25519. Without it `read_boot_root` returns `UnsupportedAlgorithm`. |
| `lz4`, `fdt`, `fit` | LZ4 segments, FDT patching, FIT parsing. |
| `sha3-digest` | SHA3-256 in crypto. Directory file digests remain SHA-256. |

## Static ACPI (`acpi_dsl!`)

- Macro: `crates/acpi-macros/src/` (`parse.rs`, `validate.rs`, `emit.rs`).
- Runtime: `crates/acpi/src/lib.rs` (`AmlFragment`, `BoundAmlFragment`) and
  `crates/acpi/src/aml_linker.rs` (`AmlWriter`).

- Literal and `const` parts are encoded at expansion, including package lengths
  and names. Use a `static` for fixed fragments.
- Runtime operands declare a width: `#{byte EXPR}`, `#{word EXPR}`,
  `#{dword EXPR}`, `#{qword EXPR}`. A bare `#{EXPR}` is rejected.
- `#{const EXPR}` is range-checked at compile time.
- `AmlWriter` writes into a caller-owned buffer with capacity checks. Errors are
  sticky, so a failed table is never finalized; a failed scope rolls back.
- Each open scope reserves 4 bytes of temporary PkgLength slack per nesting
  level; capacity must include it.
- Some tables still use the `acpi_tables` crate (e.g. `crates/acpi/src/dbg2.rs`).

Tests: `crates/acpi/tests/acpi_dsl.rs`, and `crates/acpi/tests/acpica_behavior.rs`,
which runs `iasl` (must be on `PATH`) to compile and decompile.

## Tests

```sh
cargo test -p fstart-ffs --features std
cargo test -p fstart-crypto --all-features
cargo test -p fstart-core
cargo test -p fstart-stage --features ffs,ffs-signature,lz4,fit
cargo test -p fstart-acpi -p fstart-acpi-macros --all-features
```

Coverage includes: root wire golden and reserved bytes, signed root with
compressed bytes, directory golden revision (`crates/ffs/tests/fixtures/directory-v2.bin`),
LZ4 trailing input, trust and locator round trips, ledger overlap and capacity,
policy refresh, FDT source and capacity, configured-entry mismatch.

Smoke test: `cargo fbuild run --board qemu-riscv64 --release --payload halt`
should log `qemu-riscv64 ramstage: ready for payload`. The halt payload loops,
so use a timeout. Flipping a directory byte in an assembled image should fail at
`VerifiedDirectory::open` (digest mismatch) before payload readiness.
