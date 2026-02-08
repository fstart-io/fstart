# Unified Region Model

> Replaces the old Manifest / FileEntry / RwSlotPointer / NvsPointer design.

## Motivation

The original FFS type hierarchy used four different types to describe what
are fundamentally all "named byte ranges in the image":

- `Manifest` — a file table for a region, carrying `rw_slots` and `nvs`
  fields that are only valid for RO manifests (phantom fields on RW manifests)
- `FileEntry` — a named file with segments and digests
- `RwSlotPointer` — a pointer to an RW region's manifest
- `NvsPointer` — a pointer to raw NVS storage

This created several problems:

1. **Three different pointer shapes** for the same concept (a byte range).
2. **Phantom fields** — `Manifest.rw_slots` and `Manifest.nvs` are always
   empty on RW manifests; the type system doesn't enforce this.
3. **Manual `region_base` threading** — every segment read requires the
   caller to pass the correct `region_base`, with no type-level enforcement.
4. **NVS doesn't fit** — it's neither a region (no manifest) nor a file
   (no segments). `FileType::Nvs` existed but was never used.
5. **`FfsHeader`** was dead code.

## Design

### Core Insight

A firmware image is a tree of **regions**, exactly two levels deep:

```
Root (image-level manifest)
 ├── Container "ro"        — holds files, has a signature
 │    ├── File "bootblock" — loadable binary with segments + digests
 │    ├── File "main"      — loadable binary with segments + digests
 │    └── File "board.cfg" — data blob
 ├── Container "rw-a"      — holds files, has its own signature
 │    └── File "main"      — updated firmware
 ├── Container "rw-b"      — holds files, has its own signature
 │    └── File "main"      — A/B redundant copy
 └── Raw "nvs"             — plain reserved space, no structure
```

Every node is a **Region**: a named, typed byte range. The `RegionContent`
enum discriminates what kind of region it is, and each variant carries only
the fields relevant to that kind. No phantom fields, no special pointer types.

### Type Hierarchy

```rust
/// A top-level region in the image. Lives in the root manifest.
pub struct Region {
    pub name: HString<64>,
    pub offset: u32,          // from image base
    pub size: u32,
    pub content: RegionContent,
}

/// What a top-level region contains.
pub enum RegionContent {
    /// A signed container of files.
    Container {
        children: heapless::Vec<RegionEntry, 32>,
    },
    /// Raw reserved space (NVS, scratch pads).
    Raw {
        fill: u8,
    },
}

/// A child entry within a Container region.
pub struct RegionEntry {
    pub name: HString<64>,
    pub offset: u32,          // relative to parent Region's offset
    pub size: u32,
    pub content: EntryContent,
}

/// What a child entry contains.
pub enum EntryContent {
    /// A loadable file with segments and integrity digests.
    File {
        file_type: FileType,
        segments: heapless::Vec<Segment, 8>,
        digests: DigestSet,
    },
    /// Raw reserved space within a container.
    Raw {
        fill: u8,
    },
}
```

### The Root Manifest

The root manifest replaces the old `Manifest` type. It is the single signed
structure that the anchor points to:

```rust
pub struct ImageManifest {
    pub regions: heapless::Vec<Region, 8>,
}
```

Each `Container` region has its own `SignedManifest` serialized within the
image. The root `ImageManifest` lives in the anchor's pointed-to location
and covers the overall image layout. Each Container's children are serialized
in a separate `SignedManifest` that the Container's offset/size points to.

**Wait — this adds a level of indirection.** Let's simplify.

### Simplified: Single Signed Root

For the current needs (RO-only images, no independent RW updates), the
simplest correct design is:

- The anchor points to **one** `SignedManifest`
- That manifest contains an `ImageManifest` with all `Region`s
- Each `Container` region's `children` are inline in the manifest (not
  separately signed)

For future A/B updates where RW regions need independent signing:
- The `Container` variant gains an optional `manifest_offset` / `manifest_size`
  pair, indicating a separately-signed child manifest
- The root manifest's Container children list may be empty (meaning "go read
  the separately-signed manifest at this offset")

But we do NOT build this yet. The current design inlines everything into a
single signed manifest. This is sufficient for RO-only and simple multi-stage
images.

### Anchor Simplification

The `AnchorBlock` simplifies since region-specific fields move into the
manifest:

```rust
pub struct AnchorBlock {
    pub magic: [u8; 8],          // "FSTART01"
    pub version: u32,            // bumped to 4
    pub manifest_offset: u32,    // single root manifest
    pub manifest_size: u32,
    pub total_image_size: u32,
    pub key_count: u32,
    pub keys: [VerificationKey; 4],
}
```

Gone: `ro_manifest_offset` → `manifest_offset`, `ro_manifest_size` →
`manifest_size`, `ro_region_base` (now in the Region's `offset` field).

### Segment Addressing

Segment offsets are relative to their parent `Region`'s `offset`:

```
absolute_offset = region.offset + entry.offset + segment.offset
```

The reader resolves this by walking the tree. No manual `region_base`
parameter needed — the reader returns a `ResolvedRegion` or similar
that bundles the base offset.

### What Gets Deleted

- `Manifest` (replaced by `ImageManifest`)
- `RegionRole` enum (replaced by naming convention — "ro", "rw-a", etc.)
- `RwSlotPointer` (replaced by `Region` with `Container` content)
- `NvsPointer` (replaced by `Region` with `Raw` content)
- `FfsHeader` (dead code)
- `FileType::Nvs` (NVS is a `Raw` region, not a file type)

### What Stays the Same

- `AnchorBlock` remains `#[repr(C)]`, read by volatile pointer cast
- `SignedManifest` wrapping remains (postcard-serialized manifest + signature)
- `Segment`, `SegmentKind`, `SegmentFlags`, `Compression` unchanged
- `DigestSet`, `Signature`, `SignatureKind` unchanged
- `VerificationKey` unchanged
- `FileType` (minus `Nvs` variant)

### Limits

| Container | Bound |
|---|---|
| Regions per image | 8 (`heapless::Vec<Region, 8>`) |
| Entries per container | 32 (`heapless::Vec<RegionEntry, 32>`) |
| Segments per file | 8 (`heapless::Vec<Segment, 8>`) |
| Names | 64 chars (regions/entries), 32 chars (segments) |
| Verification keys | 4 |
| Serialized manifest | 8192 bytes |

### Reader API Changes

Before:
```rust
reader.read_ro_manifest(&anchor) -> Manifest
FfsReader::find_rw_slot(&manifest, RegionRole::RwA) -> &RwSlotPointer
reader.read_rw_manifest(&slot, &anchor) -> Manifest
FfsReader::find_file(&manifest, "main") -> &FileEntry
reader.read_segment_data(&seg, region_base) -> &[u8]
```

After:
```rust
reader.read_manifest(&anchor) -> ImageManifest
FfsReader::find_region(&manifest, "ro") -> &Region
FfsReader::find_entry(&region, "main") -> &RegionEntry
reader.read_segment_data(&seg, region) -> &[u8]   // region carries its own offset
```

The `region_base` threading disappears — the reader takes a `&Region`
which carries its own `offset`.

### Builder API Changes

Before:
```rust
FfsImageConfig {
    keys, ro_region: RegionConfig { role, files },
    rw_regions: Vec<RegionConfig>, nvs_size: Option<u32>,
}
```

After:
```rust
FfsImageConfig {
    keys: Vec<VerificationKey>,
    regions: Vec<InputRegion>,
}

enum InputRegion {
    Container { name: String, files: Vec<InputFile> },
    Raw { name: String, size: u32, fill: u8 },
}
```

The builder lays out regions sequentially, builds the manifest with
computed offsets, signs it, and patches the anchor.

### FFS Version Bump

Format version bumps from 3 to 4 to reflect the incompatible manifest
structure change.

### Migration Path

This is a breaking format change. All existing FFS images become invalid.
Since no production images exist (only dev/test), this is acceptable.
The test suite is rewritten to use the new API.
