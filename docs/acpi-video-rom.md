# Video firmware handoff to the OS

Two ACPI interfaces matter here:

- **`_ROM`**, ACPI Appendix B.4.3 (“Get ROM Data”), is the standard display-device
  method. Arg0 is a byte offset and Arg1 the requested length, at most 4 KiB;
  the result is a Buffer. Place it under the PCI display function's ACPI
  companion, not its root port or an output connector.
- **VFCT** is AMD's vendor-defined video BIOS table. Linux `amdgpu`'s APU path
  tries VFCT but does **not** call `_ROM`. Implementing just the standard method
  would therefore not solve the Picasso/Raven2 handoff.

## References and coreboot comparison

- [ACPI 6.6, Appendix B.4.3](https://uefi.org/specs/ACPI/6.6/Apx_B_Video_Extensions.html#rom-get-rom-data).
  The same offset/length/Buffer contract can also be read in Appendix B.4.3 of
  [ACPI 3.0a, pages 575–576](https://www.intel.com/content/dam/www/public/us/en/documents/articles/acpi-config-power-interface-spec.pdf).
  Direct retrieval of the UEFI site's current document was blocked during this
  implementation; the older primary specification and current indexed section
  agree on the contract.
- AMD's published `UEFI_ACPI_VFCT`, `VFCT_IMAGE_HEADER`, `GOP_VBIOS_CONTENT`
  definitions in Linux `drivers/gpu/drm/amd/include/atombios.h` / `atomfirmware.h`
  define the VFCT wire ABI. The fixed table header is 76 bytes and each image
  header is **28 bytes**, followed immediately by its image data. These are not
  native Rust structure offsets or a standard ACPI Appendix B table.
- `../coreboot/src/device/pci_rom.c::pci_rom_ssdt()` retains the posted image
  (or copies an unposted one into CBMEM) and emits `_ROM` in its PCI scope.
  `src/acpi/acpigen.c::acpigen_write_rom()` uses a serialized SystemMemory
  field, limits requests to 4 KiB, and zero-pads end-of-image reads. Zero-padding
  is coreboot-compatible behavior, not an additional claim about the spec.
- `../coreboot/src/acpi/acpi_vfct.c` publishes one or more AMD ROMs with actual
  PCI identity. `soc/amd/common/block/graphics/graphics.c` also emits `_ROM`
  outside the FSP GOP path. `amdgpu_bios.c` is the relevant Linux consumer.

## Reusable implementation

`fstart_acpi::video::rom_aml(image)` emits the standard method and a private
`VDAT` buffer. It is graphics-vendor-, architecture- and executor-independent.
For example, a platform with an existing GPU node can put it in that scope:

```rust,ignore
let rom = fstart_acpi::video::rom_aml(posted_image)?;
writer.scope(r"\_SB.PCI0.GFX0", |writer| writer.raw(&rom))?;
```

The bytes are **embedded in the AML**. Neither emulator RAM nor an input slice
must survive table assembly. There is no physical-address cast, identity-map
assumption, or separately retained allocation for the OS to find. The platform
retains its ordinary ACPI table storage. `VDAT` is reserved in that device's
scope; the method is serialized because its temporary field/buffer names must
not collide between callers. Separate devices have separate data and locks.

Reads are bounded before subtraction/bit conversion, including offsets at EOF
and `u64::MAX`. Requests are capped at 4096 bytes, short/end reads are padded
with zeros, and a zero-length request returns an empty buffer. Images need not
be aligned to 4 KiB or be at least 4 KiB long. Empty images are not advertised.

`fstart_acpi::vfct::build_vfct(uuid, images)` copies one or more images into a
revision-1 SDT, using the existing SDT checksum/serialization machinery and
little-endian zerocopy headers. The caller supplies hardware identity and UUID;
there are no PCI reads, emulator dependencies or board assumptions in the
builder. It validates total length, slot/function, nonempty images and duplicate
functions. VFCT cannot describe nonzero PCI segments, so those are rejected
rather than silently mapped to segment zero. LIB1/reserved fields remain zero.

## Runtime integration

The VBIOS driver returns `InitializedDisplay`: framebuffer metadata plus an
owned copy of the **posted** image. The shadow header's size is honored within
the private 128-KiB ROM window, allowing POST to resize/patch its image. No guest
pointer escapes and no post-patch checksum is confused with asset verification.
The original executable asset is still authenticated and validated before POST.

Picasso publishes both interfaces from that same image. VFCT uses the **actual**
PCI vendor/device/subsystem identity, not the compatible ROM's PCIR device alias
(e.g. actual `15d8` versus selected ROM `15dd`). The standard method is attached
using `PciEcam::device_path()`, which walks the allocator's cached secondary-bus
parent links and rejects missing, ambiguous or cyclic paths without rescanning
configuration space. Existing AMD `GPxx` root-port nodes are extended, not
recreated with duplicate `_ADR`s. The GPU's bus and internal port are not fixed.

Device initialization now precedes ACPI publication, so the posted data and
standalone VFCT exist before the table set is built. The native AMD table arena
is 512 KiB: enough for a full 128-KiB image in both transports plus other tables.
Assembly still fails without partially copying a table set when storage is too
small. Other platforms choose their own storage and may reuse either builder
with firmware images from a different initialization path.

## Validation

The test suite includes:

- Real ACPICA execution of 28 read/mutation cases across two independent display
  devices: unaligned and cross-page reads, small images, short fields, padding,
  EOF/past-EOF/maximum offsets, zero requests, oversize requests and repeat calls.
  Returned buffers can be modified without altering the ROM or later results.
- Independent iASL disassembly/recompilation and execution under AMD's complete
  PCI namespace, including an existing root port and a function `_ADR`.
- VFCT byte offsets, actual identities, multi-image layout, checksum and invalid
  input rejection, plus publication in a full ACPI table set.
- Cached PCI hierarchy resolution and private posted-ROM extraction/resizing.
- Maximum-size dual-transport storage and undersized-buffer rejection.

Validated independently on the Wi-Fi/SCI fixes (`266dad8c`), without the
experimental S3 change: 116 ACPI unit tests plus integration/doc suites, 21 PCI
unit tests, 3 VBIOS tests and 26 AMD platform tests pass. PCI, VBIOS and platform
all-target clippy pass with warnings denied; ACPI clippy reports only existing
warnings in untouched `wake.rs`/`tock_bridge.rs`. The generic crate also passes
an `aarch64-unknown-none` cross-check with only its ARM feature enabled.

Picasso Linux and CrabEFI UI ROMs and the Zeppelin reset/SMM stages build.
Artifacts are `.local/vilboz-linux/acpi-video-{linux,uefi-ui}.rom`; hashes and
logs are `acpi-video-built.sha256` and `acpi-video-independent-*.log`. The Linux
test image selects s2idle; this change does not add S3 support.

The reviewed CrabEFI UI image was installed on 2026-10-30. Two full-chip
pre-install reads matched the preceding UI image (`e326067c…`) and an independent
recovery copy was saved outside the checkout. AP/EC reset were held during
programming and independent readback; all 16 MiB matched the intended image:
`4f4b0d72f6823a2ffd9bc18d2b4dfc586c555b2a81f277258a8b9cb0ccaa1c9b`.
Final EC `powerinfo` confirmed G3 and GSC confirmed both reset lines deasserted.

Evidence: `.local/vilboz-linux/acpi-video-install-20261030T145523Z/install.json`
and its sibling readback/control logs. Recovery is retained at
`~/.local/share/firmware-backups/vilboz/acpi-video-20261030T145523Z/before.rom`.
The Linux image has not been flashed. The user subsequently confirmed that the
installed firmware works and approved merging it. The agent did not capture an
OS graphics-driver initialization trace; user confirmation and independent
readback are distinct from such a trace.
