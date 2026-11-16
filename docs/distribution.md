# Distributing firmware without a toolchain

Building fstart needs a pinned nightly Rust, `-Zbuild-std` and a linker per
target. Assembling an image from compiled stages needs none of that. A bundle
carries everything CI compiled for one board and payload, so users can
assemble the final image with the files only they may have: a vendor BIOS
(AMD PSP firmware cannot be redistributed), a kernel, or their own signing key.

## Bundles

A bundle is one board/payload build as a ZIP (DEFLATE level 9):

- `manifest.json`: format version, board, payload, revision, the build's
  plan (its image steps, inputs and options), stage binaries, required local
  inputs and the size and SHA-256 of every member.
- `files/units/<unit>/...`: every unit output: ELF and flat stages, SMM images,
  generated ABI headers.
- Redistributable assembly inputs (microcode, VBTs, device trees) and their
  licenses.
- `files/images/...`: an image CI assembled itself, when the build needs no
  local files. It is signed with a throwaway key.

Vendor ROMs, external kernels and runtime firmware are never included; the
manifest declares them as inputs instead. An AMD build packs its PSP firmware
from blobs the user's donor image provides: the manifest carries the blob
tables (`packed_donor`), and the plan is resolved with the blob directory
assembly extracts the donor into (`bundle::BLOBS_DIR`), so the build never
reads a vendor image. Nothing else of the donor ends up in the image.

A board can name an official vendor download that works as its donor:

```toml
[package.metadata.fstart.donor]
vendor = "GIGABYTE"
name = "MJ11-EC0 BIOS F02"
url = "https://download.gigabyte.com/FileList/BIOS/server_bios_MJ11-EC0_F02.zip"
sha256 = "f02f3f42…"   # of the file as the vendor serves it
size = 13426298
terms = "https://www.gigabyte.com/WebPage/469/EULA.html"
page = "https://www.gigabyte.com/…/support#support-dl-bios"
note = "…"            # optional caveat shown to the user
```

`fbuild bundle` copies it into the catalog entry of each build that needs a
donor (`donor_source`). Nothing is downloaded at build time. The web assembler
offers it behind an explicit agreement to the vendor's terms; see the release
proxy below.

Boards and variants may also declare a display `name`; the catalog passes it
on with the board's architecture. What is a setting of a board rather than
another board (the S8030's PCIe bifurcation) is one of the build's options
(docs/options.md): a build whose options live in its flash image says so
(`settings` in the catalog), and its assembly seeds them (`--setting`).

`fbuild bundle --seabios DIR` ships a SeaBIOS build (the output of
`ci/payloads/seabios/build.sh`) in every `coreboot` bundle: SeaBIOS, its VGA BIOS
and the serial-console setting. Those builds then need no file from the user,
CI assembles their images, and a user can still supply another payload ELF.

```sh
# Every board, variant and payload its platform takes, plus catalog.json:
cargo fbuild bundle --out target/distribution --revision "$(git rev-parse HEAD)"
# One selection:
cargo fbuild bundle --board gigabyte-mj11-ec1 --payload uefi \
  --out /tmp/mj11 --revision "$(git rev-parse HEAD)"
```

The output directory must be empty. A board publishes the payloads its
platform's plan takes: every payload fbuild knows is resolved, and those the
plan rejects are not published. A build that compiles but does not fit its
flash layout is still published, with its `image_error` in the catalog.

`fbuild compose-bundle` assembles a bundle natively, without Cargo or a linker:

```sh
fbuild compose-bundle --bundle gigabyte-mj11-ec1-uefi.bundle.zip \
  --donor vendor.zip --out image/
```

Add `--input kernel=bzImage` for named inputs, `--signing-key key.pem` for
an Ed25519 PKCS#8 key (without one, a fresh key is generated and discarded),
and `--setting KEY=VALUE` to seed the image's settings store, as
`fbuild assemble --setting` does.
A donor may be a raw ROM, or a ZIP whose ROM/bin/fd images are all the same
image (vendor updates often ship one copy per flash tool).

`Bundle::assemble` (`crates/image-build/src/bundle.rs`) runs the plan's image
steps (`image_steps::execute`), as `fbuild assemble` does, over an in-memory
file tree (`files::MemoryFiles`) instead of the host filesystem. It extracts an
AMD donor's blobs with the same tables as `--donor`; a halved (SP3) flash packs
the generations the donor has. `fstart-image-build` builds for wasm32 too, for
the web assembler. Bundles are validated before use:
unknown format versions, altered or extra members, oversized inputs and missing
local inputs are rejected. The output includes `assembly-report.json` with the
revision and the hash of every input and output.

The bundle format is version 3. Older bundles must be re-exported.
