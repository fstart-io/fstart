# AMD firmware image tooling

AMD image tooling is a Rust implementation. Coverage grows incrementally, but format, platform and
board policy must remain separate from the start.

## Boundaries

- `fstart-amdfw-format`: `no_std`, MIT/Apache-2.0 wire layouts, Fletcher-32,
  the directory table writer, address resolution, EFS and boot-half
  selection, checked PSP/BIOS traversal, PSP-order file lookup (L2 first) and
  dependency validation. It uses zerocopy and has no silicon-driver
  dependency; AMD document #55758 and the SP3 reverse engineering are its
  foundations. It is the one reader of the host tools and of the firmware
  (the SP3 reset image reads its flash with it; a `Profile` per generation
  comes from `fstart_driver_zen::soc::Generation::profile`). `fstart-core`
  takes its board-facing BIOS slot type directly from this crate.
- `fstart-image-build::amd`: assembly policy and the current Picasso blob
  configuration/packer. It consumes the same reader to check vendor inputs and
  read back assembled images. Signed blobs are not modified or re-signed.
- `fstart-amdfw` CLI: shared inspection/validation plus partition packing. The
  old flag-only packing invocation remains supported; `pack` is the explicit
  equivalent.

Keep packer/configuration code out of the format crate.

## Platform and image selection

A `Profile` supplies EFS generation acceptance, BIOS slot, PSP/chip combo IDs,
legacy-offset policy and the Picasso APOB-NV alias. It is data, not an exhaustive
list of supported CPU names. `Profile::for_slot` supplies current defaults;
callers can override them for another platform.

`Flash::region` explicitly supplies:

- the entire file, so references can extend beyond a selected 16-MiB region;
- that image's origin in the file;
- its physical mapping base;
- optionally, a recovery-slot origin.

The four address modes are physical, image-relative, table-relative and
slot-relative. The effective mode is the higher of directory and entry modes.
Missing slot context is an error, not a reason to reinterpret an address.
Header v0/v1 mode and allocation fields have different layouts and are decoded
separately. Combo selection never defaults to the first entry; duplicate
matching entries are rejected. Offline combo selection by identity is **not**
root-key authentication, including for lookup-mode-0 images.

The CLI names Picasso, Zeppelin/Naples, Rome and Milan as starting profiles.
`--psp-id`, `--chip-id`, `--generation-mask`, `--image-origin`, `--physical-base`
and `--slot-origin` make selection/mapping policy explicit rather than relying
on flash-size heuristics.

```
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- \
  inspect vendor.rom --profile milan
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- \
  validate vendor.rom --profile rome
cargo run -p fstart-amdfw-tools --bin fstart-amdfw -- \
  inspect multi-image.rom --profile milan --image-origin 0x1000000
```

Inspection resolves the selected PSP and BIOS trees. Validation additionally
checks all reachable PSP combo branches, backup PSP pointers and every populated
BIOS slot of the retained EFS. Prefix validation accepts a separately declared
replacement reset range:

```
fstart-amdfw validate vendor.rom --profile zeppelin \
  --prefix-size 0x294000 --reset-offset 0xdb0000 --reset-size 0x250000
```

A corrupt referenced L2 is not an absent L2. Referenced tables must fit their
parent link; duplicate/nested L2 links are refused. Preservation includes table
allocation extents, every PSP branch, PSP-to-BIOS table links (type 0x49), and
non-reset BIOS files. ISH recovery links (0x48/0x4a) are explicitly unsupported,
not mistaken for opaque files whose dependencies need no traversal. Ancillary EFS
payload formats with unknown lengths are refused for partial-prefix assembly,
not assumed safe because their starting pointer is in the prefix. This checks
structural preservation, not cryptographic validity or hardware bootability.

## Firmware blobs

A native AMD image is packed from vendor firmware files in a blob
directory (`--blobs DIR`, workspace-relative or absolute). Two Cargo.toml
tables say which files and where they go
(`fstart_image_build::amd::blobs`):

- The SoC's, owned by the platform crate:
  `[package.metadata.fstart.amd.<soc>]` in `crates/platform-amd/Cargo.toml`.
  The PSP partition's geometry and settings, and the file of every PSP and
  BIOS directory entry, microcode patch (by CPU equivalence ID) and video
  BIOS (by PCI ID). Paths follow the layout of the `amd_blobs` repository.
- The board's: `[package.metadata.fstart.amd]` in its Cargo.toml. Its SoC,
  its APCB instances and whether its PSP needs fstart's PSP application:

  ```toml
  [package.metadata.fstart.amd]
  soc = "picasso"
  psp-app = true
  # One APCB per RAM ID, at <board>/apcb/<n>.bin in the blob directory ...
  apcb = { instances = 13 }
  # ... or generated from a template, paths in the blob directory:
  # apcb = { template = "zork/uma_128.apcb", spds = ["spd/1.hex", ...],
  #          gpio = [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]],
  #          strip-manufacturer = true }
  ```

fbuild hands both tables to the platform, which resolves them in the blob
directory. The microcode and the video BIOSes go into the FFS; the rest
into the PSP partition. The image's receipt (`<board>.json`) records every
input's SHA-256.

### From an `amd_blobs` checkout

`--blobs path/to/amd_blobs` serves the SoC's files as they are. Board files
(the APCB instances) go next to them, e.g. extracted from the board's flash
as below.

### From a vendor flash image

`cargo fbuild blobs` fills a blob directory with exactly the files the
board's build reads, at the paths it reads them from:

```sh
cargo fbuild blobs --board google-vilboz --from backup.rom --out blobs/
cargo fbuild assemble --board google-vilboz --release --blobs blobs/
```

- PSP and BIOS directory files by their entry (type, subprogram,
  instance), L2 before L1 as the PSP looks them up, the board's APCBs from
  the BIOS directory's `0x60` instances;
- microcode patches by the CPU equivalence ID in their header and the size
  that CPU family's patches have, from the BIOS directory if it lists them,
  else wherever in the image they are (e.g. a coreboot or UEFI firmware
  file system), the newest when there are several;
- video BIOSes by their legacy option ROM header and PCI data structure
  (vendor, device, x86 code, checksum) wherever they are; copies must be
  identical.

Or build from the vendor image directly: `--donor backup.rom` instead of
`--blobs` extracts the same files once per board and image content into
`target/fstart-blobs/<board>/<image SHA-256>/` and builds from there.

It writes `fstart-blobs.toml` with the source image's and each file's
SHA-256 and where it was found. A file the image doesn't have is an error,
or, with `--allow-missing`, left out and listed there. Compressed files
(e.g. a video BIOS in a compressed UEFI section) aren't found; take them
from `amd_blobs`. Extracted files are the vendor's versions, signed as they
were; nothing is modified.

## Coverage and next extensions

Current inspection covers EFS BIOS slots at +0x18/+0x1c/+0x20/+0x28, `2PSP`,
`$PSP`/`$PL2`, `$BHD`/`$BL2`, header v0/v1 and address modes 0–3. Current **blob
packing** remains Picasso-specific; this is a coverage limit, not the format
architecture. Vendor assembly still enforces its existing single-image,
top-aligned-reset board contract.

Next steps are native assembly of multi-level/combo layouts, then a newer
client's ISH/recovery-slot format. Add those as checked topology/placement
formats consumed by inspection, validation and assembly together—not as another
standalone packer or a parallel family-specific parser. Unknown cookies and
unsupported topology fail explicitly today. Extend signing metadata/placement
without treating opaque signed blobs as editable firmware code.

Use Picasso and SP3 fixtures together when evolving the format API. Synthetic
fixtures exercise header versions, all relative modes, nonzero image origins,
combo closure and corrupt-L2 rejection; vendor donor files remain external and
must not be committed. Keep newer-client differential fixtures alongside these
as their formats are implemented.
