# APCB v2 follow-up: all remaining AMD boards

The other AMD targets in this tree are Google Vilboz (Picasso) and Gigabyte
MJ11 EC0/F02 and EC1/F09 (Zeppelin). S8030's PCIe bifurcation variant uses the
same source APCBs as S8030. There are no additional APCB-based board crates in
this revision.

## Host experiment

The v3 library's version guard was **not** simply relaxed. A separate temporary
copy of `amd-apcb` 0.5.5 was used to distinguish container fidelity from missing
typed fields. Neither fstart's dependency nor these boards' assembly paths was
changed.

| Target | Inputs checked | Original declared bytes | Patched-library reconstruction | Remaining opaque records per payload |
|---|---:|---:|---|---:|
| Vilboz | all 13 RAM-ID instances | 4,672 each | 13/13 byte-identical | 6 |
| MJ11 EC0/F02 | both extracted slot representations | 6,416 each | 2/2 byte-identical | 4 |
| MJ11 EC1/F09 | both extracted slot representations | 6,416 each | 2/2 byte-identical | 4 |

This compares every declared byte, including headers and checksums. Unused slot
padding is excluded. It is **not fully typed source generation**, an assembled
firmware test, a boot test or memory-training qualification. No board was flashed.
The probe deliberately allows raw fallback so it can report precisely what is
still missing; the production v3 generator continues to reject that fallback.

## What actually failed

The unmodified library has several independent v2 problems:

1. `groups[].context` is exported but rejected on import (also seen with v3).
2. `NaplesConsoleOutControl` is exported but absent from the import visitor.
3. Parameter export omits the terminator, but import expects it to be present in
   the source vector. Merely fixing the import visitor produced a **4,640-byte**
   Vilboz output instead of **4,672 bytes**, with misplaced parameter values.
   Restoring the terminator attribute and its `0xff` value fixes this loss.
4. Legacy CBS type `0x000f` is also a parameter list, but the library treats it
   as an unknown structure.
5. MJ11 uses nine parameter IDs absent from `ParameterTokenConfig`:
   `0x0741..0x0745`, `0x0747..0x0749`, `0x074c`. Typed access panics on them.
   The probe assigns explicitly unnamed numerical placeholders, **not guessed
   hardware meanings**. All 264 parameter records can then be retained; 255 have
   existing names.
6. Both MJ11 images contain duplicate CBS `0x000d` and `0x000f` entries with the
   same header identity. The builder rejects these. The probe permits v2
   duplicates to reproduce the observed inputs; that is not a proposal to
   remove production uniqueness checks indiscriminately.
7. `struct_body` is exported for unsupported types but cannot be imported. The
   diagnostic patch adds raw import solely to measure the remaining coverage.

After those changes all 17 inputs reconstruct exactly. This establishes that
v2 is feasible; it does not justify advertising the remaining blobs as decoded.

## Typed coverage still required

### Vilboz

All **183 parameter records** have existing names. The six opaque records are:

| Memory entry | Content | Bytes |
|---|---|---:|
| `0x0030` | embedded SPD records and their descriptors | 2,164 |
| `0x0059` | SODIMM DDR4 ODT patterns | 160 |
| `0x005a` | SODIMM DDR4 command/address bus table | 180 |
| `0x005b` | SODIMM DDR4 data-bus table | 260 |
| `0x005c` | SODIMM maximum-frequency table | 32 |
| `0x005d` | SODIMM stretch-frequency table | 32 |

The existing fstart v2 SPD editor already knows the SPD descriptor. Some library
memory structures have the right-looking sizes but intentionally exclude the
SODIMM entry IDs from compatibility checks. Equal size alone is not enough to
assert the same layout or field semantics. Those types should be checked and
added upstream, not worked around with retagging or boilerplate wrappers.

### MJ11 EC0 and EC1

The four opaque memory records are:

| Memory entry | Content | Bytes |
|---|---|---:|
| `0x0043` | older UDIMM DDR4 data-bus table | 528 |
| `0x0048` | older RDIMM DDR4 data-bus table | 484 |
| `0x0052` | older error-output control structure | 108 |
| `0x0056` | older LRDIMM DDR4 data-bus table | 264 |

The data-bus payload lengths are multiples of 44 bytes, unlike the library's
newer 52-byte rows. The error-output structure is smaller than its 112/116-byte
variants. These need explicit versioned definitions, not truncation of modern
structures. The two variants have the same coverage gaps but retain their own
configuration values.

## Reproduction

No donor bytes are committed. Supply a clean checkout of `amd-apcb` at commit
`1ec66d607de2437813cdd9ffa91719a4b163b3cf` and the extracted standalone APCBs:

```sh
python3 tools/amdfw/tests/probe-apcb-v2.py /path/to/amd-apcb \
  /path/to/vilboz/apcb/*.bin /path/to/mj11-ec0-apcb*.bin \
  /path/to/mj11-ec1-apcb*.bin --offline
```

The script copies the library into a temporary directory, applies the checked-in
**diagnostic** patch, and builds a standalone probe with unwinding enabled. It
never edits the supplied checkout, donor files or production Cargo dependency.
`--offline` requires the dependencies to be cached. It prints one JSON result
per input with byte equality, opaque record IDs/sizes and unnamed parameter
counts. A mismatch, import error or typed-accessor panic returns a failing status.
A successful status with `fully_typed: false` must not be called full support.

Next step: fix v2 serialization and model the missing structures in the existing
library, then extend the production source compiler and assembly paths with the
same byte-exact and donor-compatibility checks used for v3. Until then, these
boards keep their existing vendor APCBs; the draft SP3 implementation is not
weakened to accept opaque replacements.
