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
manifest declares them as inputs instead. An AMD build packs its PSP firmware
from blobs the user's donor image provides: the manifest carries the blob
tables (`packed_donor`), platforms resolve the plan with
`BuildSelection::defer_firmware`, so the build never reads a vendor image, and
the recipe names the blob directory assembly extracts the donor into. Nothing
else of the donor ends up in the image.

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
(`files::MemoryFiles`) instead of the host filesystem. It extracts an AMD
donor's blobs with the same tables as `--donor`; an SP3 image packs the
generations the donor has. Bundles are validated before use:
unknown format versions, altered or extra members, oversized inputs and missing
local inputs are rejected. The output includes `assembly-report.json` with the
revision and the hash of every input and output.

The bundle format is version 2. Older bundles must be re-exported.

## Nightly releases

On every master push that passes the checks and QEMU boot tests, the
`distribution` CI job runs `ci/build-distribution.sh`: it builds all bundles,
`catalog.json` and `host-tools-linux-x86_64.tar.gz` (prebuilt `fbuild`,
`fstart-amdfw`, `fstart-apcb`) and keeps them as a two-day Actions artifact.
It builds SeaBIOS at the revision the boot tests pin and passes it to
`fbuild bundle --seabios`.

The daily **Nightly releases** workflow runs `fstart-release nightly`
(`tools/release`). It takes the newest successful master push run and, unless
that commit already has a nightly:

1. downloads its artifact as data (nothing downloaded is executed),
2. checks the catalog against the run's commit and every bundle's size and
   SHA-256, and rejects unexpected files,
3. adds `release.json` and `SHA256SUMS` and stamps `created_at`, `expires_at`
   and the source run into the catalog,
4. creates the prerelease `nightly-YYYY-MM-DD-<12-char commit>` as a draft and
   publishes it only after every upload finished.

GitHub Releases do not expire, so the same job deletes nightlies it created
(marked in the release notes) 30 days after publication, with their tags. The
newest nightly is kept even when expired; other releases are never touched.
A release may hold 1000 assets; publication checks that bound.

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
whose kernel slot is under 1.5 MiB as not fitting. Variants that are a setting
show as one board with a Configuration choice. For Intel images where fstart
fills only the BIOS region and leaves the descriptor, GbE and ME regions empty,
it also offers the BIOS region alone and tells the user to write only that
region. It can create an Ed25519 signing key with Web Crypto; the key is saved
to a file right away and otherwise stays in the tab.

The `distribution` job builds the site into `target/image-web` (and runs the
native/WASM parity test, `ci/test-web.mjs`); **Publish web assembler** deploys
it to Pages after a successful master CI run. Bundles stay on Releases and do
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

`ci/test-web.mjs` opens every bundle in the WASM bindings (under Node) and
compares WASM and native `compose-bundle` output byte for byte for
representative builds. Point `FSTART_TEST_DONOR` (MJ11),
`FSTART_TEST_SP3_DONOR` (Tyan S8030) or `FSTART_TEST_ETHANOLX_DONOR` at a local
vendor image to include the AMD recipes. Donors are never needed in CI and
must not be uploaded.
