# Distributing firmware without a toolchain

Building fstart needs a pinned nightly Rust, `-Zbuild-std` and a linker per
target. Assembling an image from compiled stages needs none of that. A bundle
carries everything CI compiled for one board and payload, so users can
assemble the final image with the files only they may have: a vendor BIOS
(AMD PSP firmware cannot be redistributed), a kernel, or their own signing key.

## Bundles

A bundle is one board/payload build as a ZIP (DEFLATE level 9):

- `manifest.json`: format version, board, payload, revision, the platform's
  assembly recipe, stage binaries, required local inputs and the size and
  SHA-256 of every member.
- `files/units/<unit>/...`: every unit output: ELF and flat stages, SMM images,
  generated ABI headers.
- Redistributable assembly inputs (microcode, VBTs, device trees) and their
  licenses.
- `files/images/...`: an image CI assembled itself, when the build needs no
  local files. It is signed with a throwaway key.

Vendor ROMs, external kernels and runtime firmware are never included; the
manifest declares them as inputs instead. Platforms resolve the plan with
`BuildSelection::defer_firmware`, so the build never reads a board's vendor
image and the recipe points at the donor input.

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

Boards may also declare a display `name`. A variant that is a setting of its
board rather than another board says so with `option = "..."` (the S8030's PCIe
bifurcation); the catalog passes both on (`name`, `option_of`), along with the
board's architecture, so a UI can show one board with an option.

`fbuild bundle --seabios DIR` ships a SeaBIOS build (the output of
`ci/payloads/seabios/build.sh`) in every `coreboot` bundle: SeaBIOS, its VGA BIOS
and the serial-console setting. Those builds then need no file from the user,
CI assembles their images, and a user can still supply another payload ELF.

```sh
# Every board, variant and payload its platform publishes, plus catalog.json:
cargo fbuild bundle --out target/distribution --revision "$(git rev-parse HEAD)"
# One selection:
cargo fbuild bundle --board gigabyte-mj11-ec1 --payload uefi \
  --out /tmp/mj11 --revision "$(git rev-parse HEAD)"
```

The output directory must be empty. Each platform lists the payloads it
publishes (`BuildPlan::publication_payloads`); unsupported combinations are not
guessed. A build that compiles but does not fit its flash layout is still
published, with its `image_error` in the catalog.

`fbuild compose-bundle` assembles a bundle natively, without Cargo or a linker:

```sh
fbuild compose-bundle --bundle gigabyte-mj11-ec1-uefi.bundle.zip \
  --donor vendor.zip --out image/
```

Add `--input kernel=bzImage` for named inputs and `--signing-key key.pem` for
an Ed25519 PKCS#8 key; without one, a fresh key is generated and discarded.
A donor may be a raw ROM, or a ZIP whose ROM/bin/fd images are all the same
image (vendor updates often ship one copy per flash tool).

`Bundle::assemble` (`crates/image-build/src/bundle.rs`) drives the same FFS and
AMD engines as `fbuild assemble`, reading from an in-memory file tree
(`files::MemoryFiles`) instead of the host filesystem. AMD donors are checked
against the recipe before use; packed recipes (Picasso, Zeppelin) extract their
blobs with the same tables as `--donor`. Bundles are validated before use:
unknown format versions, altered or extra members, oversized inputs and missing
local inputs are rejected. The output includes `assembly-report.json` with the
revision and the hash of every input and output.

The bundle format is version 2. Older bundles must be re-exported.
