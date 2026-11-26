# Distributing firmware without a toolchain

Building fstart needs a pinned nightly Rust, `-Zbuild-std` and a linker per
target. Assembling an image from compiled stages needs none of that. CI does
the compiling, and users assemble the final image with the files only they may
have: a vendor BIOS (AMD PSP firmware cannot be redistributed), a kernel, or
their own signing key.

```text
master CI ── fbuild bundle ──> Actions artifact (2 days)
                                   │ daily
                                   ▼
                       GitHub Release nightly-YYYY-MM-DD-<commit>
                                   │
            release proxy (CORS) ──┤
                                   ▼
   GitHub Pages web UI + WASM assembler <── vendor BIOS, kernel, key (local)
                                   │
                                   ▼
                              flashable image
```

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
tables (`packed_donor`), and its assembly extracts the donor into the blob
directory every AMD plan names (`files::BLOBS_DIR`; `fbuild assemble` reads
it from `--blobs`), so the build never reads a vendor image and its plan is the
one a local build has. Nothing else of the donor ends up in the image.

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
(`settings` in the catalog), and its assembly seeds them (`--setting`), and
for a CrabEFI build with Secure Boot (`secure_boot` in the plan's options)
its Secure Boot state and keys (`--secure-boot`).

`fbuild bundle --seabios DIR` ships a SeaBIOS build (the output of
`ci/payloads/seabios/build.sh`) in `coreboot` bundles: SeaBIOS, its VGA BIOS
and the serial-console setting. Those builds then need no file from the user,
CI assembles their images, and a user can still supply another payload ELF.
On 512 KiB BIOS windows the stock package does not fit, so the bundle instead
requires a smaller user-supplied payload ELF.

```sh
# Every board and variant with the payloads it publishes, plus catalog.json:
cargo fbuild bundle --out target/distribution --revision "$(git rev-parse HEAD)"
# One selection:
cargo fbuild bundle --board gigabyte-mj11-ec1 --payload uefi \
  --out /tmp/mj11 --revision "$(git rev-parse HEAD)"
```

The output directory must be empty. Without `--payload`, a board publishes
those of `uefi-ui` (or `uefi` where its plan has no UI build), `linux`,
`coreboot` and `elf` that its platform's plan takes. `halt` and `uefi-basic`
are for development and are not published, nor is a `linux` build whose
kernel slot is under 1.5 MiB. A board that publishes none of them is skipped.
If a build needs no local inputs, image assembly must succeed (including fit
in flash), or the export fails without writing a catalog. Builds that need a
user-supplied kernel, donor or other input are checked when the user
assembles them.

`fbuild compose-bundle` assembles a bundle natively, without Cargo or a linker:

```sh
fbuild compose-bundle --bundle gigabyte-mj11-ec1-uefi.bundle.zip \
  --donor vendor.zip --out image/
```

Add `--input kernel=bzImage` for named inputs, `--signing-key key.pem` for
an Ed25519 PKCS#8 key (without one, a fresh key is generated and discarded),
and `--setting KEY=VALUE` and `--secure-boot` (with its key files) to seed
the image's settings store, as `fbuild assemble` does.
A donor may be a raw ROM, or a vendor update ZIP: of its ROM/bin/fd members,
the image with firmware of the board's SoCs is the donor (vendor updates often
ship one copy per flash tool, besides capsules and tools). A ZIP of different
such images, or one that names a member twice, must be unpacked. `fbuild
assemble --donor` and `fbuild blobs`
take the same donors (`fstart_image_build::donor`).

For an Intel descriptor-mode board, pass `--intel-flash backup.rom` to
`compose-bundle` to produce an additional `<board>-full.rom`. Use a verified,
full raw SPI dump from the target machine, not a BIOS region, capsule or ZIP.
The detected flash capacity and all enabled FLREG ranges must match the build.
Boards select the encoding instead of guessing from the backup: existing
`IntelIfdFlashLayout::new` plans use IFD 1; `with_geometry(version, capacity,
regions)` also supports IFD 1.5 (Wellsburg) and IFD 2. Extended tables can
include FLREG0–15, including BIOS2, EC, IE, both 10GbE regions, PTT and
numbered reserved slots. Only the primary BIOS is replaced; all other bytes, including gaps, are
preserved. The report records the source hash and preserved ranges and hashes.
A layout match does not authenticate firmware or identify the board. With
`--intel-flash`, use fresh output filenames: none may already exist, so the
original backup cannot be overwritten, even through a link.

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

## Nightly releases

Once a day, the **Nightly releases** workflow (`nightly.yml`, also started by
hand) builds a nightly from the newest master commit whose CI passed:
`fstart-release pending` (`tools/release`) names it, or nothing when that
commit has a nightly already, and then nothing is built.

The `distribution` job checks that commit out and runs
`ci/build-distribution.sh`: it builds the bundles, `catalog.json`, the web
assembler and `host-tools-linux-x86_64.tar.gz` (prebuilt `fbuild` and
`fstart-amdfw`), with SeaBIOS at the revision the boot tests pin
(`fbuild bundle --seabios`). The `publish` job then runs
`fstart-release nightly` on that output; it

1. treats it as data (the tool is built from the workflow's checkout,
   nothing it publishes is executed),
2. checks the catalog against the commit and every bundle's size and
   SHA-256, and rejects unexpected files,
3. adds `release.json` and `SHA256SUMS` and stamps `created_at`, `expires_at`
   and the source run into the catalog,
4. creates the prerelease `nightly-YYYY-MM-DD-<12-char commit>` as a draft and
   publishes it only after every upload finished.

GitHub Releases do not expire, so the same job deletes nightlies it created
(marked in the release notes) 30 days after publication, with their tags. The
newest nightly is kept even when expired; other releases are never touched.
A release may hold 1000 assets; publication checks that bound. If any job in
`nightly.yml` fails, the `report-failure` job runs `fstart-release report-failure`
to open a `bug` issue (or comment on an already-open nightly failure issue) with
the failed run URL, failed job names, and target revision.

## Web assembler

`tools/image-web` is a static site on GitHub Pages:

- `src/lib.rs`: `ImageAssembler`, thin wasm-bindgen bindings over `Bundle`.
- `www/`: the UI. Plain ES modules and CSS, no build step. Assembly runs in a
  Web Worker (`assembler-worker.mjs`).
- `client.mjs`: release discovery and verified, progress-reporting downloads.
- `proxy/`: the release proxy (below).

The page lists nightly releases through GitHub's API, loads a release's
`catalog.json` and the selected bundle through the proxy, and checks the
bundle's size and SHA-256 against the catalog before opening it. The user then
supplies the donor and inputs the build declares, optionally a signing key, and
downloads the image and its report. Nothing is uploaded. A downloaded
`.bundle.zip` can also be opened directly, without network access.

When the build's catalog entry names a vendor download (`donor_source`), the
page offers to fetch it. The user must first tick an agreement to the vendor's
terms, which the page links along with the vendor's download page and any
`note`. The file then comes from the vendor through the proxy's `/donor`
route, and the page checks its size and SHA-256 against the catalog before
using it. The user can always pick their own file instead.

The page leaves out `halt` builds, which never boot, and treats a `linux` build
whose kernel slot is under 1.5 MiB as not fitting. For a build whose options
live in its flash image (`settings` in the catalog) it shows the options the
bundle's plan lists, with the board's values, and the image's settings store
starts with those the user changes (the S8030's PCIe bifurcation among them).
For bundles with an Intel descriptor layout, the page offers the BIOS region
alone by default. IFD 1, 1.5 and 2 use the same import, validation and region
download flow. The selected board supplies the expected descriptor version,
flash capacity and region map; a matching map is not a board identification.
To assemble a complete ROM, supply a full raw SPI backup as described above. The page
checks its descriptor before assembly and lets you download each original
region separately. With a matching layout the primary download is the full
ROM; the `.pflash` file still has empty non-BIOS regions.

The page can create an Ed25519 signing key with Web Crypto; the key is saved
to a file right away and otherwise stays in the tab.

The `distribution` job builds the site into `target/image-web` (and runs the
native/WASM parity test, `ci/test-web.mjs`); the nightly's `pages` job deploys
it to Pages once the release is published. Bundles stay on Releases and do
not count against Pages' 1 GB limit. Each nightly also carries the site as
`image-web.zip`, matching its bundles exactly.

Setup:

- **Settings → Pages → Source: GitHub Actions.**
- Repository variable `RELEASE_PROXY_URL`: the deployed proxy URL. The deploy
  job writes it into the site's `config.json`. Without it, the page only opens
  downloaded bundles.

### Release proxy

GitHub's release downloads redirect to an asset host that sends no CORS
headers, so a page cannot read them, and no repository setting changes that.
`proxy/worker.mjs` is a Cloudflare Worker that streams them with CORS:

```text
GET https://<worker>/<tag>/<asset>
  → https://github.com/fstart-io/fstart/releases/download/<tag>/<asset>
GET https://<worker>/donor/<tag>/<board>
  → donor_source.url of <board> in that release's catalog.json
```

It serves only the configured repository's release assets and the vendor
downloads its release catalogs name, follows only redirects to GitHub's asset
host (or, for a vendor download, within the vendor's host), forwards no
credentials, supports GET, HEAD, range requests and preflight, and stores
nothing. Origin filtering is not
authentication; the repository restriction is what keeps it from being an
open proxy. Configure `REPOSITORY` and comma-separated `ALLOWED_ORIGINS`
(origins without paths) in `proxy/wrangler.toml`.

Deploy with the manual **Deploy release proxy** workflow, which needs a GitHub
environment `cloudflare` with `CLOUDFLARE_ACCOUNT_ID` and a
`CLOUDFLARE_API_TOKEN` allowed to edit Workers scripts, or locally:

```sh
cd tools/image-web/proxy
npm ci && npm test && npm run deploy
```

Workers' free tier allows 100,000 requests a day; transfer is not billed. No
KV, R2 or custom domain is needed.

### Local development

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
bash ci/build-distribution.sh      # or: fbuild bundle for a few boards
node tools/image-web/dev-server.mjs target/image-web target/distribution 8080
```

The dev server serves the site and stands in for GitHub's API (one release,
`local`) and the proxy, routing asset requests through the real worker
handler. After editing `www/`, copy it into `target/image-web` again.

The `web` job in PR CI assembles a self-contained `qemu-q35` UEFI bundle and
a `qemu-armv7` Linux bundle with a generated flat kernel input. This checks
assembly and input provenance, not whether the fixture boots. It also assembles
MJ11 UEFI with a synthetic vendor-style update ZIP built from the board's
directory recipe and inert placeholder bytes; CI neither downloads nor
redistributes vendor firmware.
It runs the same `ci/test-web.mjs` test as the
nightly: open each bundle through the WASM bindings (under Node) and compare
WASM and native `compose-bundle` output byte for byte using the same signing
key. The nightly still validates the full catalog and compares every build
that can be assembled without additional inputs. To include the AMD recipes
locally, point `FSTART_TEST_DONOR` (MJ11), `FSTART_TEST_SP3_DONOR` (Tyan S8030),
`FSTART_TEST_ETHANOLX_DONOR`, `FSTART_TEST_S8040_DONOR` or `FSTART_TEST_ONYX_DONOR`
at a local vendor image or download. Real donors are never needed in CI and must
not be uploaded.
