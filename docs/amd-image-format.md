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
