# Authenticated boot and static ACPI

Implementation contracts for the [design proposal](https://g9dzg5i7q7ce.postplan.dev).
Fixed family flows remain the architecture; this is not a generic boot manager.

## Trust and updates

Current boards run in **development-integrity**. Their initial code, keys and
pins are not known to be protected against whole-image replacement. We still
check signatures and content before use, but do not claim hardware secure boot,
fault-injection resistance or persistent rollback protection.

An authenticated-update product must protect its initial verifier and policy or
use a documented hardware-authenticated chain. The expected image family and
keys come from the initial image's anchor, never from the untrusted root being
checked. The default minimum security version is zero. Enforcing rollback needs
trusted persistent state and a power-fail-safe update/recovery protocol; the
signed version field alone is not that mechanism.

A pinned SRAM bootstrap accepts only its packaged mainstage. Updating mainstage
therefore requires an authorized update to the initial image/pin. BROM loading
SPL does not by itself authenticate it. A/B recovery may verify several roots;
the rule is not literally one signature per boot, but no redundant signatures
for reopening one already accepted image.

## On-media objects

`crates/ffs/src/root.rs` defines an explicitly little-endian 512-byte root:

| Offset | Size | Contents |
| --- | --- | --- |
| 0 | 64 | Magic/revision, lengths, security version, family, key ID, reserved zeros |
| 64 | 160 | Optional bootstrap descriptor 0 |
| 224 | 160 | Optional bootstrap descriptor 1 |
| 384 | 64 | Directory reference: location, length, SHA-256, revision |
| 448 | 64 | Ed25519 signature over bytes 0..448 |

Bootstrap descriptors bind role, compression, source extent, initialized output
length, preferred address, entry offset, scratch requirement, and both stored
and loaded SHA-256 digests. Native `repr(C)` layout is not the wire format.
Unused descriptors and all reserved bytes must be zero; unsupported tags fail.

The anchor's historical `manifest_offset`/`manifest_size` fields now locate the
root, not the old signed envelope. The anchor has a new FFS version and an
expected image-family field. Old images must be rebuilt; there is no compatibility
parser. The builder derives the family ID from the board name.

Mainstage retains the expressive directory (names, regions, segments), with a
new revision for per-segment stored/initialized digests. A descriptor proves
provenance, not memory-write authority. Signed malformed offsets, unsupported
segments, ambiguous names, missing digest policy or arithmetic overflow fail.
The builder produces the root and directory from one layout and finalizes
patched stage inputs before signing their content hashes.

## Loading and lifetime

`fstart_stage::boot::MemoryPolicy` supplies trusted writable and reserved
physical windows. All loading checks the complete footprint before touching the
destination. Protect the active image, stack, heap, tables, handoff and temporary
arenas. Refresh the mainstage policy when table allocation changes the memory
map. `running_stage_windows()` supplies linker-derived current image/writable
reservations; a platform must add its other reservations.

- Uncompressed: read into the final destination and verify those bytes before
  entry. Stored/initialized lengths and digests must agree.
- Compressed: copy the complete stored input into disjoint scratch, verify that
  exact stable buffer, decompress it, then hash initialized destination bytes.
  The permitted buffer must cover initialized output and disjoint stored-input
  scratch. No overlapping Rust slices or unauthenticated decompression are
  permitted.
- A generic readable mmap is not proof of immutable media. Do not verify one
  read and then consume another unchecked read. Unsupported buffer budgets fail
  instead of falling back to unchecked streaming decompression.
- The complete directory is copied to bounded mainstage-owned RAM, verified,
  validated and retained. File views never borrow mutable flash metadata.
- Data assets are authenticated before their parsers run. Intel VBT consumers
  use the core verified-asset service, not their own signature reader. Failure
  of an explicitly configured file cannot fall back to legacy memory probing.

A retained load ledger reserves initialized bytes and BSS of successfully loaded
outputs across subsequent loads and policy refreshes. Regular loads are
serialized; a later validly signed file cannot overwrite an earlier verified
executable. Payload helpers preserve the actual verified entry and reject a
configured-entry mismatch instead of jumping to a separately configured address.

FDT preparation has a distinct, platform-declared source/destination capability.
The full destination capacity is withheld from executable, ordinary data and FIT
loads; only a non-executable FDT relocation may use that workspace. Source bounds,
header size, destination capacity and cumulative patch growth are checked before
writes. Preparation errors propagate without publishing a new ready pointer.
These limited FDT writes do not grant permission to mutate verified code.

`VerifiedExecutable` is returned only after successful loading and digest checks.
A ramstage self-check is not part of the authentication boundary.

## Family integration

Intel bootblock authenticates the root and postcar. Its UC-DRAM stash preserves
the assembly MTRR prefix and appends a versioned, bounded descriptor/directory
handoff. Postcar hashes ramstage before entry. Mainstage installs the inherited
directory and publishes explicit E820-derived load policy. The stash page and
bootstrap windows must remain disjoint through CAR teardown; magic/version
checks are structural checks, not cryptographic protection against hostile RAM.

Sunxi uses a build-patched 160-byte bootstrap pin in the initial image. The
assembler must finalize the pin and eGON fields before computing final image
hashes. The SRAM flow loads through the pinned descriptor after DRAM training,
without opening the full directory or running a public-key verifier in SRAM.
Actual target link and stack budgets, not the descriptor size, establish fit.

Early DRAM configuration and microcode are separate pre-root inputs. Current
Intel early microcode uses its existing anchor/vendor mechanism; this work does
not establish a stronger hardware protection or revision policy for it. DMA
isolation and protection of live RAM also remain platform responsibilities.

## Static ACPI

The existing `acpi_dsl!` byte backend is retained and hardened:

- Literal structure is encoded at macro expansion, including internal package
  lengths and names. Large fragments belong in static data.
- Const and runtime scalar operands use explicit widths. Range errors must not
  silently truncate; unsupported runtime structure does not fall back to an
  allocated builder tree.
- Variable shape is explicit composition/selection of fragments. Relative names
  are for known internal relationships; external namespace dependencies remain
  part of driver/platform contracts.
- The caller-buffer writer checks capacity and encoding errors. Finalized table
  pointers are published only after successful construction/checksums. Scope
  backpatching requires temporary prefix slack in capacity planning.
- One-time boot values use fixups. Live EC/hardware state stays live; introduce
  an NVS ABI only for genuinely shared OS-time data, with explicit layout,
  address width, lifetime, synchronization and suspend/resume ownership.
- Keep small non-AML builders. Removing `acpi_tables` wholesale is not a goal.
  MADT topology remains architecture-specific.

ACPICA acceptance and targeted method tests complement encoding fixtures.
Disassemble/recompile byte equality is useful for canonical fixtures, not a
universal definition of semantic conformance. Actual hardware power-management
behavior still needs OS boot and suspend/resume testing.

## Validation

Release is the reference configuration. Record per-stage text/rodata/data/BSS,
flat and packaged sizes, and peak live early memory. Signature counts, bytes
hashed, media passes and boot time matter alongside code size. Tests cover
malformed signed roots, mandatory digests, source mutation, output mismatch,
overlapping/protected destinations, compression limits, operand overflow and
writer exhaustion. Never infer a hardware security claim from a passing host
test or successful link. Recorded measurements and reproduction commands are in
[authenticated-boot-validation.md](authenticated-boot-validation.md).
