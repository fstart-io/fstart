# APCB TOML authoring experiment

This is an **opt-in S8030 Rome prototype**, not a production format migration.
The board build still uses the JSON sources described in [amd-apcb.md](amd-apcb.md).
Milan and APCB v2 are not enabled by this experiment. No hardware testing or
independent electrical qualification is implied.

## Inputs and implementation

- Board configuration: `boards/tyan/s8030/apcb/rome.toml`.
- Shared DDR4 electrical tables: `crates/platform-sp3/src/host/apcb/ddr4.toml`.
  Board-specific frequency limits and PPR capacity stay in the board file.
- Rust/Serde schema and compiler: `crates/image-build/src/amd/apcb_toml.rs`.
- Compact table views: `crates/image-build/src/amd/apcb_toml_rows.rs`.

Authored configuration is TOML, not Rust code or JSON. Rust derives the editor
JSON Schema using Schemars. Both inputs reject unknown fields. The Rust compiler
also checks references, table-name collisions, duplicate tokens/SPD slots,
storage widths, rank compatibility, and duplicate/empty priority selections.
Editor schema validation is not a substitute for those semantic checks, or for
hardware qualification.

The compiler lowers into the existing `amd-apcb` typed builder; it does not
implement another binary encoder. JSON values are an **internal adapter** to
upstream's private Serde representation, not imported templates. No JSON source
file or donor binary is read to build the TOML inputs.

## Authoring shape

The actual board file contains all three distinct payloads and all five directory
bindings. References carry the board mask, priority and instance separately from
their settings. For example:

```toml
records = [
  { use = "rdimm-cad" },
  { use = "rdimm-max-frequency-board-1", board-mask = 0x2 },
]
tokens = [
  { use = "board-bool", board-mask = 0x1 },
  { use = "board-bool" },
]

[token-sets.board-bool]
width = "bool"
values = [
  { MemRestoreControl = false },
]
```

Token sets allow the two identical boolean policies to be authored once while
still emitting both vendor records. The two **different** byte policies remain
separate. Empty CBS records, their priorities, ordering and instance identities
are retained rather than optimized away.

Electrical tables use enabled-rate/rank lists instead of dozens of false flags:

```toml
[tables.rdimm-cad]
kind = "rdimm-cad"
rows = [
  { slots = [1], rates = [1600], rank0 = ["single", "dual"], rank1 = ["empty"], addr-cmd = 0x393939, drive = ["30 Ω", "30 Ω", "30 Ω", "30 Ω"] },
]
```

- `slots` selects supported DIMM-slot counts; the compiler uses the library's
  bitmap type, not the array length. `rates` are DDR transfer rates in MT/s.
- CAD `drive` columns are CKE, CS/ODT, address/command, clock. Data-bus `drive`
  columns are DQ, DQS, ODT.
- ODT rows list three DIMM rank selectors and four chip-select `[read, write]`
  patterns. An empty rank-selector list is **not** the `"empty"` DIMM rank flag.
- Frequency rows use named upstream speed values such as `"Ddr3200"`, not a
  memory-clock integer. The library's constructors insert Rome's unsupported-
  voltage sentinels; those wire values are not authored. `conditions` retains
  the existing four condition words, without guessing LRDIMM semantics.
- This prototype targets nominal **1.2 V DDR4**. It does not model other voltages.

Container signatures, lengths, group/type IDs, checksums, padding and header
reserved fields are compiler/library concerns. `platform-tuning-end` emits the
required terminator and lets the builder pad it. `empty-ppr` declares initial
unused repair-log capacity (1–512 slots in this prototype); it is not an importer
for populated repair logs.

Reference defaults are instance 0, all boards (`0xffff`), and normal priority.
CAD gear-down and slow-mode flags default to false. SPD rows describe present
slots, with mux control address 0 unless specified. Overrides explicitly declare
`scope = "all"` (all sockets/channels/DIMMs); narrower scopes are not implemented.
One-off console/error/voltage, EEPROM mapping and Slink settings still use some
upstream field names: those are remaining authoring-polish opportunities.

Tokens normally use the upstream typed enums. An explicit `{ id = 0x..., value =
... }` preserves an unnamed ID or an unsupported enum value; its declared storage
width is still checked. This is not a claim that every vendor token is understood.

The donor digest is the **original** configuration's compatibility guard, not the
hash of edited output. Keep it unchanged when editing policy. Review/increment
`revision` when changing settings; the compiler does not automatically bump it
or invalidate cached memory training state.

## Try it

Generate the editor schemas (generated JSON Schemas are not configuration):

```sh
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- \
  toml-schema target/apcb-toml.schema.json
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- \
  toml-schema target/apcb-toml-tables.schema.json --tables

taplo lint boards/tyan/s8030/apcb/rome.toml \
  crates/platform-sp3/src/host/apcb/ddr4.toml
```

Build a standalone payload without a donor or JSON source:

```sh
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- build-toml \
  boards/tyan/s8030/apcb/rome.toml /tmp/s8030-rome-apcb.bin \
  --tables crates/platform-sp3/src/host/apcb/ddr4.toml --blob board
```

Other payload names are `cbs-backup` and `post-package-repair`. All CLI output
operations refuse to overwrite existing files. Remove an old generated schema
before regenerating it.

```sh
cargo test -p fstart-image-build -p fstart-platform-sp3 -p fstart-amdfw-tools
```

Tests compare every generated Rome byte and replacement mapping against the
existing sources and three independent donor digests, exercise actual settings
edits, and check malformed input. Replacement tests use the existing donor guards
and verify that unrelated image contents remain untouched. The existing 11-
payload v3 golden suite remains in place; this does **not** claim TOML support
for the other eight payloads yet.
