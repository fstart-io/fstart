# Source-built AMD APCBs

APCBs configure AMD PSP/ABL firmware, including memory initialization before the
x86 firmware runs. Source-built APCBs do not replace signed PSP/ABL, PMU, SMU or
other executable vendor firmware.

## Board sources

Board builds compile schema-validated TOML with the Rust `amd-apcb` encoder.
Binary headers, record sizes, padding, parameter terminators and checksums are
generated, not authored.

| Board / generation | Source |
| --- | --- |
| S8030 Rome | `boards/tyan/s8030/apcb/rome.toml` |
| S8030 Milan | `boards/tyan/s8030/apcb/milan.toml` |
| Ethanol-X Milan | `boards/amd/ethanolx/apcb/milan.toml` |
| Vilboz, RAM IDs 0–12 | `boards/google/zork/apcb/vilboz.toml` |
| MJ11 EC0/F02 | `boards/gigabyte/mj11/apcb/ec0.toml` |
| MJ11 EC1/F09 | `boards/gigabyte/mj11/apcb/ec1.toml` |
| Shared SP3 electrical tables | `crates/platform-sp3/src/host/apcb/ddr4.toml` |

Each named blob references semantic tables and, for APCB v3, named token sets.
Bindings specify active/backup directory slots separately from table placement
(instance, board mask and priority). V2 uses named parameters with explicit
storage widths and time points. Repeated legacy CBS records and intentionally
empty groups are retained, not merged away.

Vilboz JEDEC SPD images are named hex data assets under `[spd-images]`, distinct
from APCB record bodies. SPD descriptors are typed; omitting an image reserves
zeroed training scratch space. Packed Vilboz builds need neither an extracted
APCB template nor separate SPD files. Signed firmware inputs remain necessary.

## Tools

Build a payload without a donor APCB:

```sh
cargo run -p fstart-amdfw-tools --bin fstart-apcb-source -- build \
  boards/tyan/s8030/apcb/rome.toml /tmp/rome-apcb.bin --blob board \
  --tables crates/platform-sp3/src/host/apcb/ddr4.toml
```

V2 board files are self-contained; omit `--tables` and select the desired blob
name from the file. Generate editor schemas with `schema OUTPUT` and
`schema OUTPUT --tables`. Unknown fields, invalid enum values, missing references,
incompatible layouts and overflowing parameter values are rejected.

`decode INPUT OUTPUT` exports lossless typed diagnostic JSON; `build-json`
rebuilds that export. JSON is diagnostic interchange, not the board authoring
format. Output commands refuse to overwrite existing files.

## Image safety and verification

SP3 and MJ11 vendor-image builds retain donor SHA-256 guards, complete selector
coverage, slot-capacity checks and overlapping-slot checks. Every matching BIOS
directory reference is replaced, including both MJ11 directory levels. Bytes
outside declared APCB payloads, including slot padding, remain unchanged.
Vilboz packing emits all 13 active/backup pairs from source.

Golden tests independently pin all 11 SP3 payloads, all 13 Vilboz RAM IDs and
both MJ11 variants. Typed export/rebuild tests check representation preservation;
image tests check placement and unrelated-byte preservation. These checks are
not hardware qualification: memory training and boot have not been tested.

The existing MPL-2.0 `amd-apcb` implementation is vendored with localized legacy
layout and serialization fixes. Its pinned upstream revision, license and public
interoperability references are recorded in `vendor/amd-apcb/UPSTREAM`. No second
APCB encoder or opaque APCB-record fallback is used by the TOML compiler.
