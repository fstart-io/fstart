# AMD firmware image format

Reference for the AMD PSP flash image: where the code lives, how a flash view
resolves addresses, what validation checks, and how firmware blobs are supplied
to the build.

## Code map

| Path | Crate | Contents |
|---|---|---|
| `crates/amdfw-format` | `fstart-amdfw-format` (`no_std`) | Wire layouts (`efs.rs`, `directory.rs`, `anchor.rs`, `microcode.rs`), the `Flash` view and `Profile` (`lib.rs`), validation. No silicon-driver or packing code. |
| `crates/image-build/src/amd` | `fstart-image-build::amd` | `assembly.rs` (image composition), `mod.rs` (`PicassoImage` packer), `blobs.rs` (blob tables, extraction), `config.rs` (`fw.cfg` reader), `apcb.rs`, `psp_app.rs`. |
| `tools/amdfw` | `fstart-amdfw-tools` | Binaries `fstart-amdfw` (inspect/validate/pack) and `fstart-apcb`. |
| `tools/fbuild` | `fbuild` | `blobs` subcommand (`src/blobs.rs`), `--blobs`/`--donor`/`--image-config` (`src/payload.rs`), image output (`src/amd_image.rs`). |
| `crates/platform-amd` | `fstart-platform-amd` | SoC blob tables (`Cargo.toml`), noncar image plan (`src/noncar/host.rs`), Picasso policy (`src/picasso/mod.rs`). |
| `crates/driver-amd/src/sp3/soc/mod.rs` | `fstart-driver-amd` | `Generation::profile()` and `flash()` for SP3 (Naples, Rome, Milan). |
| `crates/platform-sp3` | `fstart-platform-sp3` | SP3 reset image; reads the flash anchor and BIOS directory via this crate. |

The format crate has no packing policy. Board, platform and tool choices stay in
`fstart-image-build` and `fbuild`.

For SoC-specific background see [amd-picasso.md](amd-picasso.md),
[amd-zeppelin.md](amd-zeppelin.md) and [amd-sp3.md](amd-sp3.md).

## Flash view (`fstart-amdfw-format`)

`Flash` is a view over image bytes with an explicit mapping. Construct it with
`Flash::region(bytes, origin, physical_base, profile)`; `Flash::new(bytes)` maps the
slice top-aligned below 4 GiB (legacy form).

| Field | Meaning |
|---|---|
| `bytes` | The whole file; references may extend past a 16 MiB region. |
| `origin` | File offset of the selected image. |
| `physical_base` | Physical address mapped to `origin`. |
| `slot_origin` | Optional recovery-slot origin (`with_slot`). |
| `profile` | `Profile` (see below). |

### Address modes

The effective mode is the larger of the directory's address mode and the entry's
own mode (`Flash::resolve`). Modes 0 to 3 are valid.

| Mode | Resolves to | Error when |
|---|---|---|
| 0 physical | `origin + (addr - physical_base)` for `addr` in `[physical_base, 4 GiB)`. Other values are raw image offsets only if `profile.legacy_offsets`. | Outside the window and legacy offsets are off. |
| 1 image-relative | `origin + raw` | Overflow. |
| 2 table-relative | `table offset + raw` | Overflow. |
| 3 slot-relative | `slot_origin + raw` | No slot origin set (never reinterpreted). |

The result must be inside `bytes`. Header versions v0 and v1 have different
allocation fields and are decoded separately (`directory::Header`).

### Profiles

`Profile::for_slot(BiosDirectorySlot)` gives the defaults. Callers can override any
field, and `fstart-amdfw --psp-id/--chip-id/--generation-mask` do so.

| `BiosDirectorySlot` | EFS generation mask | PSP ID | Notes |
|---|---|---|---|
| `Family17hModel00` (Naples/Zeppelin) | `0x0` | `0xbc090000` | |
| `Family17hModel10` (Picasso) | `0x0` | `0xbc0a0000` | APOB-NV uses the fixed 16 MiB alias. |
| `Family17hModel30` (Rome) | `0x1` | `0xbc0b0000` | |
| `Family19h` (Milan) | `0x3` | `0xbc0b0d00` | |

All defaults set `legacy_offsets = true` and `chip_id = None`.

### EFS and directories

- `Flash::efs_checked()` searches the EFS at the offsets in `efs::PROBES`, in
  order, within the selected region's 16 MiB window (`efs::WINDOW`). It accepts
  the first with signature `0x55aa55aa` and `generation & generation_mask == 0`.
  Nothing is taken from another generation's EFS.
- EFS fields used: `psp_directory`, `new_psp_directory`, `psp_backup_directory`,
  BIOS directory pointers `bios0`..`bios3` (+0x18, +0x1c, +0x20, +0x28), and the
  ancillary pointers `imc`, `gec`, `xhci`, `promontory*`.
- `Flash::psp_directories()`: a `2PSP` combo (`directory::Combo`) selects one
  branch. `id_select 0` matches `id & 0xffffff00` against `psp_id`; `id_select 1`
  matches `chip_id`. Zero or several matches are `Error::ComboSelection`. Without
  a combo the pointer is a plain `$PSP` directory.
- `Flash::bios_directories_checked(slot)`: `$BHD` directory with optional `$BL2`.
- `Levels` (`$PSP`/`$PL2`, `$BHD`/`$BL2`): level 2 is searched first, then level 1
  (`Levels::find`, `Levels::all`).
- `Directory::write_table` and `modify_entries` write tables; `fletcher32` seals
  them.

Selecting a combo branch by identity is an offline choice. It is not root-key
authentication, including for images with root-key lookup mode 0.

### Validation (`Flash::validate_prefix`)

`validate_prefix(slot, kept, reset)` checks that every reachable table and file
lies inside the first `kept` bytes. The one exception is the replacement reset
range `reset`. It rejects:

- an EFS or directory extent outside the kept bytes;
- ancillary EFS blobs (`imc`, `gec`, `xhci`, `promontory`, `lp_promontory`,
  `promontory19`) that start at or above the lowest directory, when the prefix is
  partial;
- a `RESET_IMAGE` (0x62) entry without both `RESET` and `COPY` flags;
- `RECOVERY_A` (0x48) or `RECOVERY_B` (0x4a) PSP entries (`UnsupportedTopology`);
- a truncated PSP-to-BIOS table link (0x49), or a nested level 2 inside one.

It checks all four BIOS slots of the EFS, both PSP pointers, and every combo branch.
A corrupt referenced L2 is an error, not an absent L2.

Validation checks structure only. It does not check signatures or hardware
bootability.

### Constants and helpers

| Item | Value / use |
|---|---|
| `HALF_SIZE` | 16 MiB, one flash half of a "Two 16M in 32MB" image. |
| `ROM2_BASE` | `0xff00_0000`, the x86 ROM2 window. |
| `booted_half(bytes, base, profile, halves)` | First half whose EFS the profile accepts. |
| `anchor::FlashAnchor` | `SP3FLASH` v1 record (flash size, FFS offset and FFS size) patched into the reset image. |
| `microcode::patch_size(id)` | `0xc80` for equivalence IDs `0x80xx`–`0x8fxx`; `0x15c0` for `0xa0xx`; `None` otherwise. |
| `microcode::patches(blob)` | Iterates concatenated flat patches. |

## Inspecting and packing (`fstart-amdfw`)

```sh
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- inspect vendor.rom --profile milan
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- validate vendor.rom --profile rome
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- inspect multi.rom --profile milan --image-origin 0x1000000
```

| Subcommand | Does |
|---|---|
| `inspect IMAGE --profile P` | Prints the EFS, then each PSP and BIOS directory with its entries and file ranges. |
| `validate IMAGE --profile P [--prefix-size N] [--reset-offset O --reset-size S]` | Inspect, then `validate_prefix`. A reset range is the only part allowed outside the prefix. |
| `pack` (or the legacy flag-only form) | Packs a Picasso PSP partition fragment with `PicassoImage`. Refuses to overwrite the output (`create_new`). |

`--profile` values are `picasso`, `zeppelin`, `naples`, `rome` and `milan`.
Selection and mapping options:

| Option | Meaning |
|---|---|
| `--image-origin` | Start of the image in a multi-image ROM (default 0). |
| `--physical-base` | Physical address of `origin` (default: top-mapped window, or the smaller flash size). |
| `--slot-origin` | Recovery-slot origin, required for slot-relative entries. |
| `--psp-id`, `--chip-id`, `--generation-mask` | Override the profile. |

`pack` writes a partition fragment, not a flash-ready chip image. Signed AMD
blobs are copied unchanged, never modified or re-signed.

`fstart-apcb` injects RAM-ID wiring and per-channel SPDs into a Picasso APCB v2
template (`--template`, `--output`, four `--gpio NUMBER:MUX:BANK` values, optional
`--spd-0-0`, `--spd-0-1`, `--spd-1-0`, `--spd-1-1`, `--hex`, `--strip-manufacturer`).

`fstart-apcb-source` compiles schema-validated TOML to APCB v2/v3 without a binary
template. It also supports lossless typed diagnostic export. Board sources and
image integration are described in [amd-apcb.md](amd-apcb.md).

## Firmware blobs

Vendor firmware files are described by two Cargo.toml tables, read by
`fstart_image_build::amd::blobs`. Paths in both are relative to the blob directory
and may not be absolute or contain `..`.

### SoC table: `crates/platform-amd/Cargo.toml`

`[package.metadata.fstart.amd.<soc>]` (`SocBlobs`). Currently only `picasso` exists.

| Key | Meaning |
|---|---|
| `bios-directory` | `BiosDirectorySlot` the PSP reads the BIOS directory from. |
| `efs-offset`, `partition-end` | PSP partition geometry. |
| `spi-read-mode`, `spi-speed`, `micron` | EFS SPI settings. |
| `soft-fuse` | PSP directory entry 0x0b. |
| `[[...psp]]` | `type`, optional `subprogram`/`instance`, `file`. |
| `[[...bios]]` | Same fields, for BIOS directory entries. |
| `[[...microcode]]` | `equivalence-id`, `file`. |
| `[[...vbios]]` | `vendor` (default `0x1002`), `device`, `file`. Added to the FFS as `pciVVVV,dddd.rom` (lowercase hex). |

### Board table: `boards/*/*/Cargo.toml`

`[package.metadata.fstart.amd]` (`BoardBlobs`). Example from
`boards/google/zork/Cargo.toml` (board name `google-vilboz`):

```toml
[package.metadata.fstart.amd]
soc = "picasso"
psp-app = true          # pack fstart's PSP application
apcb = { instances = 13 }
```

- `apcb = { instances = N }`: files `<board>/apcb/<n>.bin` for `n` in `0..N`, written
  to BIOS entries 0x60 and 0x68 with instance `n`.
- `apcb = { template = ..., spds = [...], gpio = [[num, mux, bank] x 4], second-channel = false, strip-manufacturer = false }`:
  an APCB generated from a template (`ApcbFiles`). `gpio` has exactly four entries.

For boards providing declarative `APCB_SOURCE` policy (including Vilboz), the
instance metadata checks generated active/backup coverage instead. Their build
plans bind neither extracted APCB files nor template/SPD files.

### Build inputs

Use one of:

| Input | Flag | Source |
|---|---|---|
| Blob directory | `--blobs DIR` | Directory laid out as the table paths. Relative to the current directory, or absolute. |
| Donor image | `--donor ROM` | Extracts into `target/fstart-blobs/<board>/<first 16 hex digits of ROM SHA-256>/` on first use, then reuses it. |

`--blobs` and `--donor` conflict. Only Picasso images are packed from them
(`ImageConfig::Packed`). Zeppelin uses a vendor image given by `--image-config`
(`ImageConfig::Vendor`), checked against the stage layout before building.

### `cargo fbuild blobs`

```sh
cargo fbuild blobs --board google-vilboz --from backup.rom --out blobs/ [--allow-missing]
```

Extraction, from the vendor image's PSP and BIOS directories:

| File | Found by |
|---|---|
| PSP and BIOS entries | Type, subprogram and instance in the table. Level 2 is searched before level 1. |
| APCBs | BIOS entry 0x60 with the board's instance numbers. |
| Microcode | CPU equivalence ID in the patch header and the size `patch_size` gives. Taken from the BIOS directory if listed there, else a raw scan of the image. The newest patch wins; two different patches for one ID is an error. |
| Video BIOS | Legacy option ROM (`55 AA`), PCIR vendor, device, code type 0, and checksum. Raw scan; several different copies is an error. |

Compressed firmware sections are not decompressed, so files inside them are not
found. Files the image lacks are an error unless `--allow-missing`, which records
them.

Output in `--out`:

- the extracted files, at their table paths, unmodified;
- `fstart-blobs.toml` (written by `fbuild`'s `blobs` module) with `source` and
  `source-sha256`, `missing = [...]`, and one `[[file]]` per file with `path`,
  `sha256` and `origin`.

## Assembling an image

```sh
cargo fbuild assemble --board google-vilboz --release --blobs blobs/
cargo fbuild assemble --board google-vilboz --release --donor backup.rom
```

`assemble` takes `--release`, `--blobs`, `--donor` and `--image-config` (the
vendor ROM for vendor-image boards). Picasso's policy is release-only
(`Policy::release_only`).

Output goes to `target/fstart-build/<board>/<release|debug>/<digest>/image/`:

| File | Content |
|---|---|
| `<board>.rom` | Whole-flash image. |
| `<board>-psp-fragment.bin` | PSP partition fragment only, not flash-ready. |
| `<board>.json` | Flash offset, length, SHA-256 of the image, EFS and reset offsets, and SHA-256 of each input (`environment`, `path`, `sha256`), plus reset and FFS hashes. |

## Current limits

- Native packing (`PicassoImage`) writes Picasso's first-generation PSP partition
  only. Multi-level directories and ISH/recovery links are not packed; validation
  rejects recovery links.
- `fstart-amdfw inspect` and `validate` accept header v0/v1 and address modes 0 to 3.
- Unknown cookies and unsupported topology return errors.
