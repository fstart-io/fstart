# Source-built AMD APCB v3

APCB is configuration consumed by the PSP's AGESA Boot Loader (ABL), including
DRAM initialization before x86 starts. Generating it does **not** replace ABL,
PSP firmware or memory training. Those signed executable blobs still come from
the vendor ROM.

S8030 (Rome and Milan) and Ethanol-X (Milan) build their APCBs from typed source:

| Input | Source |
|---|---|
| S8030 v4.04 Rome | `boards/tyan/s8030/apcb/rome.json` |
| S8030 v4.04 Milan | `boards/tyan/s8030/apcb/milan.json` |
| Ethanol-X RXM100CB | `boards/amd/ethanolx/apcb/milan.json` |
| Shared DDR4 electrical/frequency tables and initial PPR records | `crates/platform-sp3/src/host/apcb/ddr4.json` |

These are **vendor-derived configuration values**, not independently derived
board electrical characterization. No binary APCB/template is checked in.
This is a reproducible, reviewable representation of existing settings, not a
claim that they can safely be omitted, simplified or reused on another board.

## Representation and provenance

`schema: 1` source sets name each distinct payload and its BIOS-directory
selectors (`kind`, `instance`, `subprogram`). Each generation accounts for all
five selectors: base `0x68/0`, CBS backup `0x68/8`, PPR backup `0x68/9`, active CBS
`0x60/0`, active PPR `0x60/1`. L1 and L2 can reference separate copies of the
same selector; both are replaced. Active CBS and its backup are not assumed equal.

The `apcb` object uses Oxide's typed JSON schema. Tokens have names and typed
values. Structure bodies describe SPD addressing, board-instance selection,
termination, drive strengths, frequency limits and other ABL inputs. An entry
may reference a shared typed body with `"table": "rdimm-ddr4-cad-bus"`. Table
references are explicit, not an implicit board or CPU-family default.

The three base payloads contain respectively 294, 265 and 204 token records.
Their remaining unnamed token IDs are retained numerically; typed does not mean
that every field's hardware semantics is independently understood. The smaller
CBS layers have additional unnamed token IDs. Reserved fields, board masks,
priorities and instance IDs are retained, including the vendor's board-ID maps.

There are 11 distinct payloads (3 Rome, 4 S8030 Milan, 4 Ethanol-X Milan).
Normal unit tests rebuild them without any vendor files and compare their
SHA-256 against independent golden digests of the original declared contents.
The source values come from S8030 `8030V404.ROM` and the Ethanol-X readout whose
identity is documented in `boards/amd/ethanolx/vendor/README.md`. Sizes and hashes
exclude unused directory-slot padding.

## Build integration and safety

Boards list their JSON with `Sp3BoardFacts::APCB_SOURCES`. The host plan compiles
it to bytes; firmware has no JSON parser. `sp3_flash` places those bytes at the
APCB slots resolved through the donor's actual AMD directories. The original
slot capacity, unused padding, directories and signed executable blobs stay in
place. Mirrored boot banks receive the same generated configuration.

Every source payload has a `donor_sha256` of the **original** configuration.
This is a compatibility guard, not the expected hash of an edited source output.
A changed source can produce different settings, but it will only be applied to
the reviewed donor configuration. A configured flash backup, another vendor
version or another board's image may therefore be rejected. Review and add its
source/mappings rather than bypassing the guard. For deliberate configuration
changes, also review/increment the affected `header.unique_apcb_instance` (the
vendor's configuration revision); the compiler does not silently change it or
presume that cached training state remains valid.

Assembly refuses missing or unused selectors, duplicate mappings, conflicting
slot aliases, unrecognized donor contents and outputs larger than their slots.
All APCB mappings for a half are checked before any of them is written. An
unsupported upper half remains untouched. Boards without source sets retain
their existing vendor-copy behavior.

`amd-apcb` owns the binary layouts, typed field encoding and checksum handling.
Its revision is pinned (MPL-2.0, used as a dependency). A small adapter removes
an upstream export-only `groups[].context` property and preserves original entry
order, which the upstream builder otherwise changes in CBS layers. It rejects
unknown raw structures, unsupported headers and typed exports that do not
round-trip exactly. It is not a second binary APCB parser.

## Tools and tests

The v2 Picasso SPD/template editor `fstart-apcb` is unchanged. The separate v3
tool operates on one standalone APCB, not a complete ROM:

```sh
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- decode apcb.bin apcb.json
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- build apcb.json rebuilt.bin
```

`build --tables PATH` supplies shared tables for a document using references.
To build a named payload directly from a checked-in board source set:

```sh
cargo run -p fstart-amdfw-tools --bin fstart-apcb-v3 -- build \
  boards/tyan/s8030/apcb/rome.json /tmp/s8030-rome-apcb.bin --blob board \
  --tables crates/platform-sp3/src/host/apcb/ddr4.json
```

Both operations validate before creating the output and refuse to overwrite an
existing file. `decode` only succeeds for a lossless typed export. The pinned
upstream library can abort on unsupported enum bit patterns: this is development
tooling for reviewed v3 configuration, not a hardened arbitrary-firmware parser.

```sh
cargo test -p fstart-image-build -p fstart-platform-sp3 -p fstart-amdfw-tools
```

Besides golden payloads, tests cover genuine source edits, checksums, slot
padding, missing/duplicate mappings, donor mismatch and capacity errors. When
S8030 analysis donors are available, integration tests compare complete composed
images against the old vendor-copy path. Set `FSTART_ETHANOLX_DONOR` to the
reviewed full RXM100CB readout to also test its mirrored 64 MiB image. Those
outputs are byte-identical with the unmodified sources. No new hardware qualification is implied. Changes to
memory topology/electrical policy need separate cold-boot and DIMM-population
testing with external recovery available.

Picasso/Vilboz and Zeppelin/MJ11 v2 generation is deliberately not migrated here:
unknown parameters and structure variants need additional typed-library support.
