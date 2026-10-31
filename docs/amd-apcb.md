# AMD APCB board policy

Board policy is separate from firmware ABI/default templates. Users configure
memory features, supported populations and transfer rates, and RAM-ID-to-SPD
selection. They do not specify token widths, record IDs, parameter time points,
group order, priorities, header sizes, termination, padding or checksums.

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

Unspecified settings inherit the firmware profile's defaults. Each supplied
module/training/board-ID speed-limit set replaces that population policy;
DIMM counts and rate encodings are derived. `maximum-mt-s` uses physical DDR
transfer rates, not firmware enum values or unsupported-rate sentinels.

Vilboz policy additionally selects named JEDEC SPD images with `[[ram]]` entries
containing `id` and `spd`. Descriptor fields, validity flags, channel placement,
training scratch buffers and active/backup copies belong to the profile.

## Sources

| Board / generation | Editable policy |
| --- | --- |
| S8030 Rome | `boards/tyan/s8030/apcb/rome.toml` |
| S8030 Milan | `boards/tyan/s8030/apcb/milan.toml` |
| Ethanol-X Milan | `boards/amd/ethanolx/apcb/milan.toml` |
| Vilboz | `boards/google/zork/apcb/vilboz.toml` |
| MJ11 EC0/F02 | `boards/gigabyte/mj11/apcb/ec0.toml` |
| MJ11 EC1/F09 | `boards/gigabyte/mj11/apcb/ec1.toml` |

The adjacent `templates/` directories are internal, reviewed firmware ABI/default
profiles, not the authoring interface. The private template compiler and Rust
policy mappings choose representation. Shared SP3 electrical defaults remain in
`crates/platform-sp3/src/host/apcb/ddr4.toml`. Normal board builds supply the
profile and shared defaults automatically.

Only verified semantic mappings are public. In particular, Picasso parameter
IDs must not be given Zeppelin meanings merely because their numbers coincide.
Unverified Picasso parameters, LRDIMM frequency-condition meanings and advanced
electrical settings currently remain inherited profile defaults, not guessed
public knobs. Adding a setting requires a verified firmware-family mapping and
preservation tests; users never have to choose a storage width to use it.

## Tools and safety

`fstart-apcb-source schema OUTPUT` generates the policy editor schema. The schema
rejects firmware-layout fields as well as unknown or incorrectly typed policy.
For low-level standalone builds, `build POLICY OUTPUT --template PROFILE` accepts
an explicit internal profile, plus `--tables SHARED` for SP3 and `--blob NAME` to
select a payload. Normal users build through the board's `fbuild` integration.
`decode` and `build-json` are lossless diagnostic interchange, not policy
configuration. Output commands refuse overwrites.

The existing MPL-2.0 `amd-apcb` encoder handles binary layout and bookkeeping;
there is no second encoder. Donor SHA-256, selector completeness, slot capacity
and alias guards remain in vendor-image assembly. Both MJ11 directory levels
and all active/backup mappings are preserved. Packed Vilboz builds need neither
APCB templates extracted from a ROM nor separate SPD input files.

Golden tests pin all 11 SP3 payloads, all 13 Vilboz RAM IDs and both MJ11 variants.
Policy tests require unchanged defaults to reproduce those same bytes, and
semantic edits to retain storage and donor guards. Image tests cover placement
and unrelated-byte preservation. These are not hardware qualification: no
flashing, boot or memory-training testing has been performed. Signed executable
AMD PSP/ABL and other vendor firmware remain necessary.

The encoder's pinned provenance and public interoperability references are in
`vendor/amd-apcb/UPSTREAM`.
