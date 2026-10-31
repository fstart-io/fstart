# Browser assembly and nightly binaries

The browser assembler is a **headless SDK**, not a website. The UI can be built
separately against the generated TypeScript bindings and `client.mjs`.

## Distribution

Nightly promotion, retention and SDK ZIP packaging use the Rust `fstart-release`
tool in `tools/release`; the publisher calls GitHub's authenticated `gh` CLI.

1. Master CI must pass the normal checks and QEMU boot tests.
2. The `distribution` job discovers every board **and named variant**, asks each
   platform for its supported public payload selections, and builds all of them
   with `--release`. This includes the complete unit graph: ELF/flat stage
   binaries, SMM images and generated ABI headers. Any declared build failure
   fails distribution; boards are not silently omitted.
3. Each selection becomes `<board>-<payload>.bundle.zip`, using ZIP DEFLATE at
   level 9. The archive contains readable `files/units/...` binary paths,
   redistributable inputs, licenses, and `manifest.json`. No Cargo cache, rlibs,
   compiler snapshots, private keys or vendor ROMs are published. External
   kernels/runtime firmware are declared as local inputs, not copied from a
   developer's board directory.
4. Where no donor or external input is required and native assembly succeeds,
   the archive also includes images under `files/images/`. If placement fails
   (for example, an existing board build exceeds its flash capacity), all its
   compiled binaries are still published and `catalog.json` records an explicit
   `image_error`. Such a build is not advertised as flashable. Preassembled
   images use a fresh ephemeral signing key, not a project release-signing identity.
5. `catalog.json` lists every board/payload asset, compressed size, SHA-256,
   required inputs and binary paths. `image-web.zip` contains the versioned WASM
   SDK, JavaScript loader, TypeScript declarations and headless client helper.
   `host-tools-linux-x86_64.tar.gz` contains prebuilt native fbuild/amdfw tools.
6. The scheduled Nightly releases workflow promotes the most recent successful
   **master push CI run**, without rebuilding or executing downloaded code.
   Publication validates the catalog against that run's commit and verifies
   bundle hashes. A release remains a draft until all assets have been uploaded.

Nightly tags are immutable `nightly-YYYY-MM-DD-<12-char-commit>`. The daily job
publishes nothing if that revision is already available; manual dispatch can
publish another revision sooner. `release.json` and the catalog contain UTC
`created_at`/`expires_at` timestamps and the source workflow run ID.

GitHub Releases have **no automatic expiration**. The daily job deletes only
expired, explicitly marked nightlies and their tags after 30 days. Stable or
unrelated releases are untouched. The newest successful nightly is kept even
past its advertised expiry until a replacement exists. An interrupted draft can
be inspected and deleted before retrying the same tag. Actions handoff artifacts
have only two-day retention: they are a short-lived handoff, not the public
archive.

GitHub currently allows 1000 assets per release; publication checks that bound.
For now there is one asset per board/payload, not a separate release per board.

## Pages: SDK only

`Publish WASM SDK` deploys only the small SDK artifact from a successful master
push CI run. Board bundles remain on Releases, so they do not count against
Pages' 1 GB site limit. There is intentionally no index page or website design.
When a UI is added, its build should be incorporated in this same Pages artifact
rather than deployed by a competing workflow.

Enable **Settings → Pages → Source: GitHub Actions**. The GitHub Pages environment
must allow the SDK workflow to deploy from the default branch.

## Cloudflare proxy

Release downloads redirect to an asset host without browser CORS permission.
`tools/image-web/proxy/worker.mjs` is a small streaming bridge:

```text
GET https://<worker>.workers.dev/<release-tag>/<asset-filename>
                     ↓
https://github.com/fstart-io/fstart/releases/download/<tag>/<asset>
```

It allows only the configured repository, validates redirect destinations,
forwards no credentials, and supports GET/HEAD/range requests and preflight. It
streams the response; it does not buffer firmware, assemble images or store data.
No GitHub token is needed for public assets. CORS origin filtering is **not**
authentication; the repository restriction prevents use as an arbitrary proxy.

Configure `REPOSITORY` and comma-separated `ALLOWED_ORIGINS` in
`tools/image-web/proxy/wrangler.toml` (defaults: `fstart-io/fstart` and
`https://fstart-io.github.io`). Use the site's origin, without a path. Add a custom
Pages domain here if applicable.

Create a GitHub environment named `cloudflare` with:

- `CLOUDFLARE_ACCOUNT_ID` — target account ID.
- `CLOUDFLARE_API_TOKEN` — scoped to editing Workers scripts in that account.

Then manually run **Deploy release proxy**, or deploy locally:

```sh
cd tools/image-web/proxy
npm ci
npm test
npm run deploy
```

Account credentials are intentionally not committed. The Worker URL is supplied
by the frontend at runtime; no hostname is invented by the SDK. Workers Free
has a daily request/CPU limit; paid usage is metered. No KV, R2, paid zone or
custom domain is required. See Cloudflare's current pricing and service terms
before relying on any particular traffic allowance.

## Browser API (no UI)

```js
import init, { ImageAssembler } from './pkg/fstart_image_web.js';
import { releases, catalog, bundle } from './client.mjs';

await init();
const proxy = 'https://YOUR-WORKER.YOUR-SUBDOMAIN.workers.dev';
const [release] = await releases();
const available = await catalog(proxy, release.tag_name);
const build = available.builds.find(b =>
  b.board === 'gigabyte-mj11-ec1' && b.payload === 'uefi');
const assembler = new ImageAssembler(await bundle(proxy, release.tag_name, build));
try {
  const manifest = JSON.parse(assembler.manifest());
  // The UI supplies local file bytes; these are never uploaded.
  assembler.setDonor(vendorZipOrRomBytes);
  // For Linux builds: assembler.setInput('kernel', kernelBytes);
  // Other required inputs and capacities are in manifest.inputs.
  const outputs = assembler.assemble(undefined); // Map<filename, Uint8Array>
  const rom = outputs.get('gigabyte-mj11-ec1.rom');
  // outputs also includes assembly-report.json: revision, donor/input hashes,
  // and the size/SHA-256 of each generated image.
  // The UI can offer rom and its report as Blob downloads.
} finally {
  assembler.free();
}
```

Run expensive assembly in a Web Worker to avoid blocking the UI. Explicitly
choose a build; hardware compatibility is not inferred from the machine running
the browser. `binary(logicalPath)` reads a compiled binary or preassembled image
using its path from the manifest/catalog.

`assemble(optionalPkcs8KeyBytes)` signs using an Ed25519 PKCS#8 key supplied by the
user, or a fresh ephemeral key using browser randomness. A project release
private key must **never** be embedded in the SDK. Hashes detect mismatched or
corrupt inputs; they do not authenticate vendor firmware or establish a secure
boot root independently of how the firmware was installed.

Raw donors and ZIPs with exactly one ROM/bin/fd image are accepted; ambiguous
archives must be unpacked by the user. Packed recipes, including MJ11 and
Picasso, use the existing table-driven extractor and single-/two-level PSP
packers. Donor-derived microcode is inserted before FFS hashing/signing.
Vendor-prefix recipes validate the dependencies they retain. All AMD recipes,
including SP3's selected or packed banks and mirrors, use the same
`amd::assembly` compositor and stage-metadata patcher in native and WASM builds.
SP3 vendor composition retains unsupported banks unchanged; other layouts do
not inherit its dual-bank assumptions. There is no new PSP parser.

The current bundle/catalog format is version 2 (the unified AMD recipe).
Version-1 bundles must be re-exported; they are not silently migrated.
Bundles are versioned and validated before use. Unknown versions, altered
members, duplicate/oversized ZIP inputs and missing required local inputs fail
closed. They are unpacked into memory, not extracted to host filesystem paths.
Output placement is the same as native fbuild, not a new guarantee that an image
is appropriate to flash over board-specific provisioning or protected regions.

## Local builds and tests

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
bash ci/build-distribution.sh
```

This produces all release assets in `target/distribution/` and the Pages SDK in
`target/image-web/`. It runs proxy/client/retention tests, opens every bundle in
the actual WASM bindings, and checks native/WASM output equality for
representative x86 and ARM/eGON builds using the same explicit test key.

For a single selection:

```sh
cargo fbuild bundle --board gigabyte-mj11-ec1 --payload uefi \
  --out /tmp/mj11-bundle --revision "$(git rev-parse HEAD)"
```

The output directory must be empty. Omitting `--board` selects every base board
and variant; omitting `--payload` selects every payload declared by its platform.
The native tools can consume the same bundle without invoking Cargo or a linker:

```sh
fbuild compose-bundle --bundle gigabyte-mj11-ec1-uefi.bundle.zip \
  --donor vendor.zip --out image/
```

Use `--input kernel=bzImage` or `--signing-key key.pem` as needed. For local MJ11
native/WASM parity testing, set `FSTART_TEST_DONOR` to a local ZIP/ROM when running
`ci/test-web.mjs` against a catalog containing MJ11 halt/UEFI bundles. This tests
all available non-Linux MJ11 selections. `FSTART_TEST_SP3_DONOR` enables the
same checks for Tyan S8030 dual-bank bundles; `FSTART_TEST_ETHANOLX_DONOR` enables
Ethanol-X boot-bank/mirrored-readout checks. Donors are never needed for public
CI and must not be uploaded as artifacts.
