# Register data

This is the source of truth for the migrated ICH7/NM10 and i945 register
catalogs. It captures **the hardware knowledge already used by fstart**, not
complete chipset datasheets. Initialization sequences, training tables and
board policy remain in Rust.

## Regenerate

From the repository root:

```sh
cargo register-gen
cargo register-gen --check
cargo test -p fstart-register-gen
```

Commit YAML changes and their generated Rust together. CI checks that the
outputs are current. Firmware consumes committed Rust: YAML parsing, chiptool
and the generator are **host tools only**, not firmware dependencies. Generation
uses the repository's pinned Rust toolchain, including rustfmt.

## Descriptions versus bindings

`intel/*.yaml` uses the existing **chiptool register IR**, as used by
[ch32-data](https://github.com/ch32-rs/ch32-data):

- `block/Name`: offsets, access widths, access direction and register arrays.
- `fieldset/Name`: fields, bit positions, widths and enum references.
- `enum/Name`: named numeric field values.
- `extends`: reuse a verified block or fieldset layout across revisions.

`catalogs.yaml` selects outputs and Rust bindings. A block can emit:

- A Tock overlay, with an explicit extent and optional shared PCI Type 0 header.
- An offset module for existing sparse BAR access. Arrays also emit `_STRIDE`
  and `_COUNT` constants.
- Both, from the same description.

The backend emits `fstart_core::mmio` register types, not new volatile-access
helpers. PCI headers reuse `fstart_pci::pci_type0_config!`. Runtime BAR selection,
ECAM access, barriers and register ownership stay with the existing code. There
are no fixed device base addresses in the generator.

The first slice deliberately retains the Tock API. Using YAML does not require
rewriting drivers or introducing a new hardware-access trait. Offset-only
blocks are not MMIO pointer constructors; a future SMN or indexed-register
binding must use its existing transport rather than treat offsets as pointers.

## Migration rules

- Keep widths, masks, enums and access sequences unchanged. Preserve unknown
  fields and numeric whole-word programming rather than inventing meanings.
- `bit_size` records the existing primary access view. Some sequences also use
  narrower accesses; an offset constant does not prohibit those views.
- Legacy `RAW` and timing-assembly fields overlap intentionally. This migration
  preserves them; it does not certify their widths against a new datasheet.
- A read/write register type is not permission to use read-modify-write on
  write-one-to-clear status, strobes or other side-effect registers. Keep exact
  writes and existing handling in the driver.
- Do not merge layouts merely because names match. Share only verified IP
  revisions, with chipset-specific differences explicit.
- Add independent wire-layout/encoding tests for migrated blocks. The ICH7/i945
  `register_tests.rs` files cover offsets, widths, arrays, masks, enums and
  preservation of neighboring bytes using RAM-backed register windows.

The Tock backend currently supports scalar fields and flat blocks. Contiguous
register arrays can be overlaid; sparse arrays are supported in offset-only
catalogs. Unsupported shapes, invalid references, inheritance cycles, malformed
fields and invalid overlay layouts are rejected rather than silently rewritten.
