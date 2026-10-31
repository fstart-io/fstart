# AMD APCB policy, platform ABIs and board data

The encoder, platform ABI and mainboard choices have separate owners:

- Vendored `amd-apcb` owns binary structures and serialization.
- `crates/platform-amd-apcb` owns reusable Rome/Milan/Picasso/Zeppelin ABI
  knowledge: token identities/storage, parameter order, required records,
  placement and representation. It contains no mainboard registry or BIOS-release
  dispatch. The platform selects the ABI.
- Each mainboard owns its semantic policy, actual tuning/default values, SPD
  assets and firmware provenance. Vendor release names are audit data, not ABI
  selectors.
- `image-build` consumes generated replacements and performs generic guarded
  image assembly. It does not select a mainboard compiler preset.

Adding a mainboard using a supported ABI requires board data, not changes to
build-tool dispatch. Tests exercise an unseen board identity and changed board
values without changing compiler code.

## Semantic policy

For example:

```toml
schema = 1
[memory]
ecc = true
restore-context = false
speed-limits = [
  { module = "rdimm", slots-per-channel = [2], dual-rank-dimms = 2, maximum-mt-s = 2133 },
]
```

Users specify memory features, physical populations and transfer rates, and
RAM-ID-to-SPD selection—not token widths, groups, priorities, record construction,
time points or bookkeeping. The ABI derives storage and placement. Each supplied
module/training/board-ID speed-limit set replaces that population policy.
Unspecified settings retain board defaults.

| Board | Semantic policy | Board compatibility/provenance data |
| --- | --- | --- |
| S8030 Rome | `boards/tyan/s8030/apcb/rome.toml` | `rome.compat.toml` alongside it |
| S8030 Milan | `boards/tyan/s8030/apcb/milan.toml` | `milan.compat.toml` alongside it |
| Ethanol-X Milan | `boards/amd/ethanolx/apcb/milan.toml` | `milan.compat.toml` alongside it |
| Vilboz | `boards/google/zork/apcb/vilboz.toml` | `compat.toml` alongside it |
| MJ11 EC0/EC1 | `boards/gigabyte/mj11/apcb/ec0.toml`, `ec1.toml` | One shared `compat.toml`, with data-local `ec0`/`ec1` identities |

## Explicit compatibility data

Some vendor settings are not understood well enough to expose as semantic
controls. They remain explicitly labelled decoded compatibility **values** with
the board, not hidden platform defaults or an executable template language.
These files contain no token widths, record lists, layout kinds or placement
instructions. Parameter IDs are numeric where semantics are unverified; the
platform catalog determines storage and order. Physical channel wiring is data;
the ABI constructs the corresponding records and termination.

Compatibility data is transitional. Investigate undocumented values rather than
inventing names. In particular, Picasso parameter IDs must not be assigned
Zeppelin meanings just because the numbers coincide. Unsupported semantic
settings are rejected. Shared physical-board defaults can be reused across
firmware variants; variant lookup occurs within the supplied board data, never
in a global mainboard registry.

## Tools and verification

Board builds supply policy/data and select the platform ABI automatically.
Standalone use is board-agnostic:

```sh
fstart-apcb-source build POLICY OUTPUT --abi zeppelin --board-data COMPAT --variant ec1
```

`--blob NAME` selects a logical payload. `schema OUTPUT` generates the semantic
policy editor schema; `decode`/`build-json` provide typed diagnostic interchange.
Output files are never overwritten.

Donor SHA-256, selector completeness, slot capacity and alias checks remain in
image assembly. All matching active/backup slots and both MJ11 directory levels
are handled. Vilboz packing binds no extracted APCB/template/SPD files.

Independent goldens cover 11 SP3 payloads, 13 Vilboz RAM IDs and both MJ11
variants. Tests preserve representation and unrelated image bytes, and verify
semantic changes do not change storage or donor guards. These are not hardware
qualification: no flashing, boot or memory-training tests have been performed.
Signed executable AMD firmware remains required. Encoder provenance and public
interoperability references are in `vendor/amd-apcb/UPSTREAM`.
